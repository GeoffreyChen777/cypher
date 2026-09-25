// Boot splash — port of the desktop's (loaders.rs splash_overlay): the compact
// ASCII cypher wordmark over an opaque backing, a tracked "LOADING" line, and
// `splash-out` (150ms hold, then 0.5s fade + 6px lift) once there's real
// content underneath.
//
// Mobile adds the entrance: the wordmark decrypts — each glyph flickers
// through scrambled indigo characters before locking into ink, the front
// sweeping left to right. Cold launch only (foregrounding never shows it);
// static under reduced motion.

import SwiftUI

enum BootSplash {
    /// crates/ui/assets/loading-wordmark.txt, verbatim.
    static let wordmark: [String] = #"""
                            _
      ___   _   _   _ __   | |__     ___   _ __
     / __| | | | | | '_ \  | '_ \   / _ \ | '__|
    | (__  | |_| | | |_) | | | | | |  __/ | |
     \___|  \__, | | .__/  |_| |_|  \___| |_|
            |___/  |_|
    """#.components(separatedBy: "\n")

    static let columns = wordmark.map(\.count).max() ?? 0

    // ---- decode timing (seconds from the splash's first frame) ----
    /// The appear front crosses every column in this long.
    static let sweep = 0.36
    /// Per-glyph jitter on the front, so it reads as a ragged edge.
    static let frontJitter = 0.05
    /// How long a glyph scrambles before locking (plus up to `scrambleJitter`).
    static let scramble = 0.22
    static let scrambleJitter = 0.08
    /// A freshly locked glyph flashes to full ink, then settles to 70%.
    static let lockFlash = 0.12
    /// Scrambled glyphs re-roll at ~18Hz — fast enough to read as noise, slow
    /// enough not to strobe.
    static let rerollInterval = 0.055
    static let decodeDuration = sweep + frontJitter + scramble + scrambleJitter + lockFlash

    /// Never hold the app back longer than this, even with nothing to show
    /// yet (offline first launch): Home's own connection states take over.
    static let maxWait = 3.0

    /// splash-out (motion.rs SPLASH_OUT): EASE over 500ms after a 150ms hold.
    static let exitAnimation = Animation.timingCurve(0.25, 0.1, 0.25, 1, duration: 0.5).delay(0.15)
    static let exitLift: CGFloat = 6

    private static let scramblePool = Array("_/\\|()<>[]{}-=+*#%&$@01")

    enum Glyph: Equatable {
        /// Not reached by the front yet — a space, so the grid holds.
        case blank
        case scrambled(Character)
        /// `flash` runs 1 → 0 over `lockFlash` after locking.
        case locked(flash: Double)
    }

    static func glyph(row: Int, col: Int, char: Character, at t: Double) -> Glyph {
        if char == " " { return .blank }
        let appear = Double(col) / Double(max(columns - 1, 1)) * sweep
            + unit(row, col, salt: 1) * frontJitter
        let lock = appear + scramble + unit(row, col, salt: 2) * scrambleJitter
        if t < appear { return .blank }
        if t < lock {
            let tick = UInt64(max(0, t / rerollInterval))
            let ix = Int(unit(row, col, salt: 3 &+ tick) * Double(scramblePool.count))
            return .scrambled(scramblePool[min(ix, scramblePool.count - 1)])
        }
        return .locked(flash: max(0, 1 - (t - lock) / lockFlash))
    }

    /// Deterministic 0..<1 per (row, col, salt) — splitmix64's finalizer.
    static func unit(_ row: Int, _ col: Int, salt: UInt64) -> Double {
        var x = (UInt64(row) &* 0x9E37_79B9_7F4A_7C15)
            ^ (UInt64(col) &* 0xBF58_476D_1CE4_E5B9)
            ^ (salt &* 0x94D0_49BB_1331_11EB)
        x ^= x >> 31
        x = x &* 0xD6E8_FEB8_6659_FD93
        x ^= x >> 32
        return Double(x % 10_000) / 10_000
    }

    // ---- when it shows ----

    /// Test and screenshot rigs drive specific screens from the first frame;
    /// `-splash` forces it on for them.
    static func enabled(arguments: [String] = ProcessInfo.processInfo.arguments,
                        environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        if arguments.contains("-splash") { return true }
        if environment["XCTestConfigurationFilePath"] != nil { return false }
        return !arguments.contains { ["-demo", "-e2e", "-bench"].contains($0) }
    }

    /// Something real is underneath: sign-in resolved, and a signed-in
    /// workspace has either its on-device cache (hydrated synchronously —
    /// local-first) or a live room. A first launch after sign-in has neither
    /// until the hello lands.
    static func contentReady(restored: Bool, phase: AppModel.Phase, demo: Bool,
                             connected: Bool, hasRows: Bool) -> Bool {
        guard restored else { return false }
        switch phase {
        case .signedOut, .pickingOrg: return true
        case .ready: return demo || connected || hasRows
        }
    }
}

/// Decode time driven by presented frames, not the wall clock: each frame
/// counts for at most `maxStep`. Launch blocks the main thread (restore, the
/// first Home layout under the splash) — a wall clock would run the whole
/// decode inside that stall and reveal a finished mark; this one just pauses.
@MainActor
@Observable
final class SplashClock: NSObject {
    static let maxStep = 1.0 / 30

    private(set) var elapsed: Double = 0
    let duration: Double
    var finished: Bool { elapsed >= duration }
    @ObservationIgnored private var link: CADisplayLink?
    @ObservationIgnored private var lastTimestamp: CFTimeInterval?

    init(duration: Double) {
        self.duration = duration
    }

    static func advance(_ elapsed: Double, by dt: Double) -> Double {
        elapsed + min(max(dt, 0), maxStep)
    }

    func start() {
        guard link == nil, !finished else { return }
        let link = CADisplayLink(target: self, selector: #selector(tick))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    /// The display link retains its target; always called on the way out.
    func stop() {
        link?.invalidate()
        link = nil
    }

    @objc private func tick(_ link: CADisplayLink) {
        if let lastTimestamp {
            elapsed = Self.advance(elapsed, by: link.timestamp - lastTimestamp)
        }
        lastTimestamp = link.timestamp
        if finished { stop() }
    }
}

struct BootSplashView: View {
    let ready: Bool
    let onFinished: () -> Void

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var clock = SplashClock(duration: BootSplash.decodeDuration)
    @State private var timedOut = false
    @State private var exiting = false

    private static let fontSize: CGFloat = 12
    private static let lineHeight: CGFloat = 14.5
    private static let wordSize: CGFloat = 11
    private static let tracking = wordSize * 0.32

    private var decoded: Bool { reduceMotion || clock.finished }

    var body: some View {
        ZStack {
            // The static launch screen's color, so the hand-off is seamless.
            Color("LaunchBackground").ignoresSafeArea()
            VStack(spacing: 28) {
                wordmark
                Text("LOADING")
                    .font(.custom("Geist-Regular", fixedSize: Self.wordSize))
                    .tracking(Self.tracking)
                    // Tracking trails the last letter too; re-center.
                    .padding(.leading, Self.tracking)
                    .foregroundStyle(Theme.textMuted.opacity(0.7))
                    // Only once the decode has finished and we're still
                    // waiting — a warm launch never flashes the word.
                    .opacity(decoded && !ready ? 1 : 0)
                    .motionAnimation(Motion.fadeIn, value: decoded && !ready)
            }
            .offset(y: exiting && !reduceMotion ? -BootSplash.exitLift : 0)
        }
        .opacity(exiting ? 0 : 1)
        // The app is live the moment the exit starts.
        .allowsHitTesting(!exiting)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Cypher")
        .accessibilityValue(ready ? "" : "Loading")
        .onAppear { if !reduceMotion { clock.start() } }
        .onDisappear { clock.stop() }
        .task {
            try? await Task.sleep(for: .seconds(BootSplash.maxWait))
            timedOut = true
        }
        // Decided here, not in the task: a task's closure keeps the view
        // value it started with, so its `ready` would be the pre-restore one.
        .onChange(of: canExit, initial: true) { if canExit { exit() } }
    }

    private var canExit: Bool { decoded && (ready || timedOut) }

    private var wordmark: some View {
        let t = decoded ? .infinity : clock.elapsed
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(BootSplash.wordmark.indices, id: \.self) { row in
                Text(line(row, at: t))
                    .frame(height: Self.lineHeight, alignment: .leading)
            }
        }
        // Ligatures off (Theme.monoUI) — the grid is one advance per char.
        .font(Font(Theme.monoUI(Self.fontSize)))
        .fixedSize()
    }

    private func line(_ row: Int, at t: Double) -> AttributedString {
        var out = AttributedString()
        for (col, char) in BootSplash.wordmark[row].enumerated() {
            switch BootSplash.glyph(row: row, col: col, char: char, at: t) {
            case .blank:
                out += AttributedString(" ")
            case .scrambled(let noise):
                var run = AttributedString(String(noise))
                run.foregroundColor = Theme.accent.opacity(0.8)
                out += run
            case .locked(let flash):
                var run = AttributedString(String(char))
                run.foregroundColor = Theme.text.opacity(0.7 + 0.3 * flash)
                out += run
            }
        }
        return out
    }

    private func exit() {
        guard !exiting else { return }
        let exit = reduceMotion ? Animation.easeOut(duration: 0.3) : BootSplash.exitAnimation
        withAnimation(exit) {
            exiting = true
        } completion: {
            onFinished()
        }
    }
}
