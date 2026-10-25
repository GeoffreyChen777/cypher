// Run progress showcase: the working "Streaming veil" session carries a long
// turn's narrated progress, and while it is open a new reading lands every
// few seconds — the fill glides, stages grow in, the steps to come refresh —
// so the status strip's bar can be seen moving offline.

import Foundation

extension DemoDataset {
    static let runProgressChatId = "chat-veil"

    /// The readings the replay walks through, then holds on the last.
    static let runProgressReadings: [RunProgress] = {
        let plan = [
            "Read the desktop veil", "Port the span math", "Wire the transcript rows",
            "Cover it with tests", "Check reduced motion",
        ]
        var stages: [ProgressStage] = []
        var readings: [RunProgress] = []
        for (ix, (label, percents)) in zip(plan, [[8, 14], [22, 31, 38], [46, 55], [64, 72, 79], [88]]).enumerated() {
            stages.append(ProgressStage(label: label, percent: percents[0]))
            for percent in percents {
                readings.append(
                    RunProgress(
                        percent: percent, current: label, next: Array(plan[(ix + 1)...]),
                        stepsLeft: ix == 0 ? plan.count : nil, stages: stages))
            }
        }
        return readings
    }()

    /// Starts the replay for the showcase session (once its store opens).
    func playRunProgressStory() {
        progressTask?.cancel()
        let startedAt = sessions[Self.runProgressChatId]?.startedAt
        progressTask = Task { [weak self] in
            for reading in Self.runProgressReadings {
                guard let self, !Task.isCancelled,
                    let row = self.sessions[Self.runProgressChatId],
                    row.status == .working, row.startedAt == startedAt
                else { return }
                self.sessions[Self.runProgressChatId]?.progress = reading
                self.sessions[Self.runProgressChatId]?.updatedAt = nowMs()
                try? await Task.sleep(nanoseconds: 4_000_000_000)
            }
            // Hold on the last reading, keeping the row fresh.
            while let self, !Task.isCancelled,
                self.sessions[Self.runProgressChatId]?.startedAt == startedAt
            {
                self.sessions[Self.runProgressChatId]?.updatedAt = nowMs()
                try? await Task.sleep(nanoseconds: 10_000_000_000)
            }
        }
    }
}
