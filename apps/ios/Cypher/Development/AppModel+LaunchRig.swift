// Demo launch rig: `-demo` launch arguments pick a route, sheet, synthetic
// transcripts and scripted animations for screenshots and benchmarks.

import Foundation
import SwiftUI

extension AppModel {
    /// Applies the `-demo` launch arguments (after the debug-rig overrides).
    func applyDemoLaunchArguments(_ args: [String]) {
        enterDemoMode()
        if args.contains("-demo-many") { demo?.addManyProjects() }
        if let ix = args.firstIndex(of: "-route"), ix + 1 < args.count {
            let spec = args[ix + 1]
            if spec.hasPrefix("chat:") {
                let chatId = String(spec.dropFirst("chat:".count))
                launchRoute = .chat(chatId)
                if args.contains("-big"), let demo {
                    // Scroll-settle stress. Injected BEFORE the transcript
                    // appears, which is the warm-session case: rows are
                    // already there at first layout, so neither the
                    // rows-arrived nor the streamed-growth anchor ever
                    // fires and `.task` is the only thing holding the
                    // bottom — against hundreds of lazily-estimated rows.
                    demo.sessionStore(for: chatId)
                        .setEntries(BenchRunner.syntheticEntries(turns: 120))
                }
                if let ix = args.firstIndex(of: "-turns"), ix + 1 < args.count,
                    let turns = Int(args[ix + 1]), let demo
                {
                    // A session of any length, e.g. for the turn scrubber.
                    demo.sessionStore(for: chatId)
                        .setEntries(BenchRunner.syntheticEntries(turns: turns))
                }
                if args.contains("-huge"), let demo {
                    // Warm-reopen stress at real-conversation scale — the
                    // estimated-height error grows with row count, and the
                    // settle must converge against it.
                    demo.sessionStore(for: chatId)
                        .setEntries(BenchRunner.syntheticEntries(turns: 600))
                }
                if args.contains("-stream"), let demo {
                    // Screenshot rig: kick off the scripted streaming reply.
                    let store = demo.sessionStore(for: chatId)
                    Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 2_000_000_000)
                        store.demoResponder?("Show me the streamed reply path.", false)
                    }
                }
            } else if spec.hasPrefix("space:") {
                launchRoute = .space(String(spec.dropFirst("space:".count)))
            }
        }
        if let ix = args.firstIndex(of: "-sheet"), ix + 1 < args.count {
            launchSheet = args[ix + 1]
        }
        launchAutosend = args.contains("-autosend")
        launchFocusComposer = args.contains("-focuscomposer")
        // Rig: open with an EMPTY transcript that lands in bulk 2.5s
        // later — the live checkpoint-backfill shape (loader → reveal).
        if args.contains("-hydrate-late"), case .chat(let lateId)? = launchRoute, let demo {
            let store = demo.sessionStore(for: lateId)
            let full = store.entries
            store.setEntries([])
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 2_500_000_000)
                store.setEntries(full)
            }
        }
        // Animation rig: "-archive-after chatId:secs" / "-unarchive-after
        // chatId:secs" fire the same animated mutation the swipe actions
        // use, so the list hand-off can be recorded headless.
        func scheduledToggle(_ flag: String, archived: Bool) {
            guard let ix = args.firstIndex(of: flag), ix + 1 < args.count else { return }
            let parts = args[ix + 1].split(separator: ":")
            guard parts.count == 2, let secs = Double(parts[1]) else { return }
            let chatId = String(parts[0])
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: UInt64(secs * 1_000_000_000))
                withAnimation(Motion.resort) {
                    if archived { self.archive(chatId: chatId) } else { self.unarchive(chatId: chatId) }
                }
            }
        }
        scheduledToggle("-archive-after", archived: true)
        scheduledToggle("-unarchive-after", archived: false)
    }
}
