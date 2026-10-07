// Synthetic transcript for the `-demo` stress routes (`-big`, `-turns`,
// `-huge`): a session doc the size of a long agent transcript.

import Foundation
import Loro

@MainActor
enum BenchRunner {
    /// A big synthetic transcript for the `-big` demo route — stresses the
    /// scroll-settle path with far more lazy rows than the demo dataset has.
    static func syntheticEntries(turns: Int) -> [MessageEntry] {
        SessionStore.decodeEntries(from: buildDoc(turns: turns)) ?? []
    }

    // MARK: Synthetic doc (schema.rs shape — see SessionStore.entryFrom)

    private static func buildDoc(turns: Int) -> LoroDoc {
        let doc = LoroDoc()
        let messages = doc.getList(id: "messages")
        for i in 0..<turns {
            let user = try! messages.pushContainer(child: LoroMap())
            try! user.insert(key: "id", v: "u\(i)")
            try! user.insert(key: "role", v: "user")
            try! user.insert(key: "createdAt", v: Int64(i * 1000))
            try! user.insert(key: "deviceId", v: "bench")
            try! user.insert(key: "status", v: "complete")
            let uparts = try! user.insertContainer(key: "parts", child: LoroList())
            try! addText(to: uparts, id: "t0", text: "Turn \(i): the ref dropdown still hangs on open — dig into it.")

            let bot = try! messages.pushContainer(child: LoroMap())
            try! bot.insert(key: "id", v: "a\(i)")
            try! bot.insert(key: "role", v: "assistant")
            try! bot.insert(key: "createdAt", v: Int64(i * 1000 + 1))
            try! bot.insert(key: "deviceId", v: "dev-mac")
            try! bot.insert(key: "status", v: "complete")
            let aparts = try! bot.insertContainer(key: "parts", child: LoroList())
            try! addText(to: aparts, id: "t0", text: prose(i))
            for t in 0..<4 {
                try! addTool(to: aparts, id: "k\(i).\(t)", index: i * 4 + t)
            }
            try! addText(to: aparts, id: "t1", text: closing(i))
        }
        return doc
    }

    private static func addText(to parts: LoroList, id: String, text: String) throws {
        let p = try parts.pushContainer(child: LoroMap())
        try p.insert(key: "id", v: id)
        try p.insert(key: "kind", v: "text")
        try p.insert(key: "text", v: text)
    }

    private static func addTool(to parts: LoroList, id: String, index: Int) throws {
        let p = try parts.pushContainer(child: LoroMap())
        try p.insert(key: "id", v: id)
        try p.insert(key: "kind", v: "tool")
        try p.insert(key: "isError", v: index % 17 == 0)
        let call = try p.insertContainer(key: "call", child: LoroMap())
        switch index % 4 {
        case 0:
            try call.insert(key: "kind", v: "exec")
            try call.insert(key: "command", v: "rg -n 'refDropdown' src/components --glob '!*.test.ts'")
        case 1:
            try call.insert(key: "kind", v: "readFile")
            try call.insert(key: "path", v: "src/components/refs/RefDropdown.tsx")
        case 2:
            try call.insert(key: "kind", v: "editFile")
            try call.insert(key: "path", v: "src/components/refs/useRefIndex.ts")
        default:
            try call.insert(key: "kind", v: "search")
            try call.insert(key: "pattern", v: "loadRefs\\(")
        }
    }

    /// A realistic assistant block: headings, prose, a list, a table, code.
    private static func prose(_ i: Int) -> String {
        """
        ## Pass \(i): where the dropdown stalls

        The dropdown's open handler awaits `loadRefs()` **before** it paints, so
        the menu can't render until the full ref index resolves. On a repo with
        many refs that's a visible hang, and it is paid again on every open
        because the result is never memoized between mounts.

        Three things stack up here:

        1. `loadRefs()` walks every ref and builds a fresh array each call
        2. The handler `await`s it inline instead of rendering an empty menu
        3. `useRefIndex` has no cache, so remount re-does the whole walk

        | Stage | Cost | Cached |
        | --- | --- | --- |
        | `loadRefs` | O(refs) | no |
        | `useRefIndex` | O(refs) | no |
        | paint | O(visible) | n/a |

        > The fix is to paint first and fill in — the index can arrive late.

        ```ts
        const refs = useRefIndex()          // memoized, suspense-free
        useEffect(() => { void warmRefIndex() }, [])
        return <Menu items={refs ?? []} loading={refs == null} />
        ```
        """
    }

    private static func closing(_ i: Int) -> String {
        """
        Landed the pass-\(i) change behind `refIndexCache`. Open latency drops to
        a paint, and the index warms in the background on first hover.
        """
    }
}
