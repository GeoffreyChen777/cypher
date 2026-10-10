// Composer — the floating glass shell in the t3 mobile composer's shape: a
// collapsed capsule (editor + send circle) that morphs into an expanded card
// with a toolbar ROW below it (attach circle · scrolling chips · pinned send)
// when the editor takes focus. Carries the desktop's Send→Steer→Stop
// semantics: live run + text = steer (same up-arrow), live run + empty = stop.
//
// Expansion is focus-driven like t3's, with the old deterministic content
// triggers kept as a floor (attachments, newline, >26 chars) — content-size
// measurement oscillates at the boundary, so it is never measured.

import SwiftUI

struct ChipOverflow: Equatable {
    var leading = false
    var trailing = false
}

/// Shared glass shell + input + action row. `chips` render in the expanded
/// toolbar row between the attach and send circles.
struct ComposerShell<Chips: View>: View {
    @Binding var draft: String
    /// Changes only after an accepted send, never on individual keystrokes.
    var editorRevision = 0
    /// See ComposerDraft.replace.
    var caretRequest = 0
    var placeholder = "Message"
    var sendEnabled: Bool
    var showStop: Bool
    var busy = false
    var hasComments = false
    /// New-session composers stay expanded — the picker chips ARE the page.
    var alwaysExpanded = false
    /// Hold the expanded layout while a picker sheet is up: presenting the
    /// sheet blurs the editor, and collapsing on that blur flaps the
    /// transcript's bottom inset mid-presentation (t3 derives expansion from
    /// focus OR sheet-active for exactly this reason).
    var keepExpanded = false
    var onSend: () -> Void
    var onStop: () -> Void = {}
    /// Staged image attachments (attachment-ui.tsx AttachmentStrip inside the
    /// pill). Non-empty forces the expanded layout, like focus.
    var attachments: [StagedAttachment] = []
    /// Present the photo picker; nil hides the attach button.
    var onAttach: (() -> Void)? = nil
    var onRemoveAttachment: (String) -> Void = { _ in }
    /// Screenshot rig (-focuscomposer): take keyboard focus shortly after
    /// appearing, so the keyboard-up transcript states can be driven headless.
    var autoFocus = false
    /// The chat's context reading, drawn round the send button; nil draws
    /// nothing (a new session, a side chat, no reading yet).
    var contextGauge: ContextGauge? = nil
    /// `@` completion; nil leaves mention chips display-only.
    var mentions: MentionEditor? = nil
    @ViewBuilder var chips: Chips

    @State private var focus = ComposerFocus()
    @State private var editorID = "composer-editor-\(UUID().uuidString)"
    /// Which ends of the chip row have content scrolled past them — each
    /// fades out only while there's more to reveal on that side.
    @State private var chipOverflow = ChipOverflow()

    private var editing: Bool { focus.isFocused }

    private var expanded: Bool {
        alwaysExpanded || keepExpanded || editing || !attachments.isEmpty || hasComments
            || draft.contains("\n") || draft.count > 26
    }

    /// One animatable shape for background/glass/hairline: capsule-radius
    /// collapsed (46pt tall pill), 20pt card expanded (t3's 999↔20 morph).
    private var surfaceShape: RoundedRectangle {
        RoundedRectangle(cornerRadius: expanded ? 20 : 24)
    }

    // Switching between VStack/HStack via AnyLayout (rather than an if/else
    // that swaps container types) keeps `input`'s view identity stable across
    // the compact↔expanded flip — an if/else here would tear down and rebuild
    // the editor, dropping keyboard focus mid-type.
    private var shellLayout: AnyLayout {
        expanded
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 0))
            : AnyLayout(HStackLayout(alignment: .center, spacing: 12))
    }

    var body: some View {
        surface
            // Focus-widen: margins pull in slightly while typing (chat-session.tsx).
            .padding(.horizontal, editing ? 10 : 16)
            .motionAnimation(Motion.resize, value: editing)
            .motionAnimation(Motion.collapse, value: expanded)
            .task(id: editorRevision) {
                guard editorRevision > 0 else { return }
                // Reattach keyboard focus to the new editor, not the retired
                // UIKit instance. This task never writes the draft.
                focus.isFocused = false
                await Task.yield()
                guard !Task.isCancelled else { return }
                focus.isFocused = true
            }
            .onAppear {
                guard autoFocus else { return }
                Task { @MainActor in
                    try? await Task.sleep(nanoseconds: 1_500_000_000)
                    focus.isFocused = true
                }
            }
    }

    /// The glass surface: collapsed = editor + send in one capsule row;
    /// expanded = attachment strip over a tall editor, with the control row
    /// (attach circle · scrolling chips · pinned send) along the card's
    /// bottom edge — everything stays inside the glass.
    private var surface: some View {
        shellLayout {
            if expanded, !attachments.isEmpty {
                AttachmentStripView(attachments: attachments, remove: onRemoveAttachment)
                    .padding(.bottom, 10)
                    .disabled(busy)
            }
            input
                .padding(.leading, expanded ? 4 : 13)
                .padding(.trailing, expanded ? 4 : 0)
                .padding(.vertical, expanded ? 4 : 5)
                .frame(minHeight: expanded ? 64 : nil, alignment: .topLeading)
                // The native editor caps at seven lines and scrolls inside;
                // nothing it draws may reach the control row below it, even
                // mid-morph while the card's layout animates.
                .clipped()
            if expanded {
                HStack(spacing: 8) {
                    if onAttach != nil {
                        attachButton
                    }
                    // Chips scroll; the send button stays pinned.
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            chips
                                .disabled(busy)
                        }
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier("composer-model-controls")
                    }
                    .scrollClipDisabled(false)
                    .onScrollGeometryChange(for: ChipOverflow.self) { geo in
                        let x = geo.contentOffset.x + geo.contentInsets.leading
                        let maxX = geo.contentSize.width - geo.containerSize.width
                        return ChipOverflow(leading: x > 1, trailing: x < maxX - 1)
                    } action: { _, new in
                        withAnimation(.easeOut(duration: 0.18)) { chipOverflow = new }
                    }
                    // Soft edges instead of a hard clip where chips slide
                    // under the attach / send circles.
                    .mask(chipEdgeMask)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    gaugedActionButton
                }
                .padding(.top, 8)
            } else {
                gaugedActionButton
            }
        }
        // 12pt right of the send button in both forms, room for the context
        // arc clear of the button and the edge; the button doesn't slide
        // sideways as the pill expands.
        .padding(.leading, expanded ? 12 : 5)
        .padding(.trailing, 12)
        .padding(.vertical, expanded ? 12 : 5)
        .background(whiteAlpha(0.04), in: surfaceShape)
        // A tall expanded card sits over transcript rows; tint its glass so
        // the text underneath can't read through the editor and control row
        // (the collapsed pill stays plain glass).
        .glassEffect(expanded ? .regular.tint(Theme.surface.opacity(0.72)).interactive()
                              : .regular.interactive(), in: surfaceShape)
        .overlay(surfaceShape.strokeBorder(whiteAlpha(0.05), lineWidth: 1))
        // The whole glass surface focuses the editor, not just the TextField's
        // own text box: the collapsed pill is mostly padding, and a tap that
        // misses the text box falls through to the transcript underneath —
        // whose tap-to-blur then RESIGNS the keyboard. That's the "have to
        // press a few times" miss. Buttons and chips still win their taps.
        // Masked to .subviews while focused so cursor-placement taps inside
        // the editor stay fully native.
        .contentShape(surfaceShape)
        .gesture(TapGesture().onEnded { focus.isFocused = true },
                 including: editing ? .subviews : .all)
    }

    private var chipEdgeMask: some View {
        let fade: CGFloat = 22
        return HStack(spacing: 0) {
            LinearGradient(colors: [.black.opacity(chipOverflow.leading ? 0 : 1), .black],
                           startPoint: .leading, endPoint: .trailing)
                .frame(width: fade)
            Rectangle().fill(.black)
            LinearGradient(colors: [.black, .black.opacity(chipOverflow.trailing ? 0 : 1)],
                           startPoint: .leading, endPoint: .trailing)
                .frame(width: fade)
        }
    }

    private var input: some View {
        ComposerTextInput(text: $draft, focus: focus, editorID: editorID, enabled: !busy,
                          placeholder: placeholder, caretToEnd: caretRequest, mentions: mentions)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .topLeading) {
                if draft.isEmpty {
                    Text(placeholder)
                        .font(Theme.sans(16))
                        .foregroundStyle(Theme.textFaint)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            // A live Steer does not transition the session's running state.
            // Explicitly retire the native editor's cached text on success.
            .id(editorRevision)
    }

    private var attachButton: some View {
        Button {
            onAttach?()
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 40, height: 40)
                .background(whiteAlpha(0.06), in: Circle())
                .overlay(Circle().strokeBorder(whiteAlpha(0.08), lineWidth: 1))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(busy)
    }

    /// Attachments count as content: an image-only send is a send, never a stop.
    private var hasContent: Bool {
        CommentPrompt.hasSendContent(text: draft, attachmentCount: attachments.count,
                                     commentCount: hasComments ? 1 : 0)
    }

    /// The send button with the context arc round it, and its long press
    /// opening the reading. The menu sits outside the button's own disabled
    /// state: an empty draft, when the reading matters most, disables send.
    private var gaugedActionButton: some View {
        actionButton
            .overlay {
                if let contextGauge {
                    ContextArc(usage: contextGauge.usage, expanded: expanded)
                }
            }
            .modifier(ContextGaugeMenu(gauge: contextGauge))
    }

    private var actionButton: some View {
        Button {
            if showStop, !hasContent {
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                onStop()
            } else {
                UIImpactFeedbackGenerator(style: .light).impactOccurred()
                onSend()
            }
        } label: {
            Group {
                if busy {
                    ProgressView()
                        .controlSize(.small)
                        .tint(Theme.bg)
                } else if showStop, !hasContent {
                    RoundedRectangle(cornerRadius: 3.5)
                        .fill(Theme.bg)
                        .frame(width: 12, height: 12)
                } else {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(buttonActive ? Theme.bg : Theme.textFaint)
                }
            }
            .frame(width: 40, height: 40)
            .background(buttonActive ? AnyShapeStyle(Theme.text) : AnyShapeStyle(whiteAlpha(0.10)),
                        in: Circle())
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!buttonActive)
        .motionAnimation(Motion.fadeQuick, value: showStop)
    }

    private var buttonActive: Bool {
        if showStop, !hasContent { return true }
        return sendEnabled && hasContent && !busy
    }
}
