// `-demo-history`: sessions spread from minutes to weeks old, for the view
// menu (SessionListMenu). Every "Last active" stop drops something, creation
// dates disagree with activity so Date created ≠ New activity, titles start
// all over the alphabet, and an old session waiting on an answer shows that
// live sessions survive the filter.

import Foundation

extension DemoDataset {
    func addSessionHistory() {
        let now = nowMs()
        let hour: Int64 = 3_600_000
        let day = 24 * hour
        let config = ChatConfig(
            harness: "pi", model: "demo/pi",
            reasoning: "high", sandbox: "workspace-write")

        // Quiet for weeks: gone under any window up to 14d.
        let archive = Space(
            id: "space-archive-tools", deviceId: "dev-mac",
            path: "/Users/dev/Projects/archive-tools", name: nil, gitDetected: true,
            gitCheckedAt: now, checkoutId: nil, createdAt: now - 60 * day)
        // Just added, no sessions yet: kept by every window.
        let fresh = Space(
            id: "space-fresh-idea", deviceId: "dev-vps",
            path: "/srv/fresh-idea", name: nil, gitDetected: false,
            gitCheckedAt: now, checkoutId: nil, createdAt: now - hour / 2)
        let research = Space(
            id: "space-research", deviceId: "dev-vps",
            path: "/srv/research-notes", name: nil, gitDetected: true,
            gitCheckedAt: now, checkoutId: nil, createdAt: now - 25 * day)
        let cypher = spaces.first { $0.id == "space-cypher" }
        spaces += [archive, fresh, research]

        // (id, title, space, device, active ago, created ago)
        let rows: [(String, String, Space?, String, Int64, Int64)] = [
            ("hist-refactor", "Long-running parser refactor", cypher, "dev-mac", 3 * hour, 40 * day),
            ("hist-yaml", "YAML front matter edge cases", cypher, "dev-mac", 5 * hour, 6 * hour),
            ("hist-benchmark", "Benchmark the syntax highlighter", cypher, "dev-mac", 60 * hour, 3 * day),
            ("hist-icons", "Icon set audit", cypher, "dev-mac", 9 * day, 9 * day),
            ("hist-onboarding", "Onboarding copy pass", cypher, "dev-mac", 35 * day, 36 * day),
            ("hist-migrate", "Migrate the release script", archive, "dev-mac", 20 * day, 21 * day),
            ("hist-changelog", "Changelog generator", archive, "dev-mac", 45 * day, 50 * day),
            ("hist-zotero", "Zotero export cleanup", research, "dev-vps", 4 * hour, 12 * day),
            ("hist-arxiv", "arXiv digest for the week", research, "dev-vps", 36 * hour, 2 * day),
            ("hist-citations", "Citation style fixes", research, "dev-vps", 6 * day, 7 * day),
            ("hist-tables", "Results tables to LaTeX", research, "dev-vps", 12 * day, 24 * day),
            // Ten days quiet but waiting on an answer: stays under every window.
            ("hist-waiting", "Which dataset split?", research, "dev-vps", 10 * day, 11 * day),
            ("hist-quick-regex", "Regex for log lines", nil, "dev-mac", hour / 3, hour / 3),
            ("hist-quick-units", "Unit conversion check", nil, "dev-vps", 3 * day, 3 * day),
            ("hist-quick-gift", "Gift ideas list", nil, "dev-mac", 15 * day, 15 * day),
            ("hist-loose-ssh", "SSH config tidy-up", nil, "dev-mac", 8 * hour, 2 * day),
            ("hist-loose-disk", "Disk usage report", nil, "dev-vps", 18 * day, 18 * day),
        ]
        for (id, title, space, device, active, created) in rows {
            // Quick chats live in the host's scratch folder; other project-less
            // sessions sit anywhere outside a project.
            let quick = space == nil && id.hasPrefix("hist-quick")
            let cwd = space?.path ?? (quick ? "/Users/dev/.cypher/cypher-scratch/\(id)" : "/Users/dev")
            chats.append(
                Chat(
                    id: id, deviceId: device, title: title, archived: false,
                    cwd: cwd, branch: space == nil ? nil : "main", checkoutId: nil,
                    config: config, lastMessagePreview: "Done.",
                    lastMessageAt: now - active, createdAt: now - created,
                    spaceId: space?.id, lastSeenAt: now - active))
        }
        sessions["hist-waiting"] = SessionRow(
            chatId: "hist-waiting", deviceId: "dev-vps",
            status: .awaitingInput, startedAt: now - 10 * day, updatedAt: now - 10_000)
    }
}
