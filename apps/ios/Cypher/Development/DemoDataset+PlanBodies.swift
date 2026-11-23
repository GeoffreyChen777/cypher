// Long plans (`-demo-long-plans`, docs/design/plan-body.md): Atlas gains a
// proposed plan whose instruction and step details were cut to fit its row,
// and a finished one whose result was, with their full text in the
// workspace chat doc's `planBodies` map the way a host writes it. Open them
// with `-route bot:bot-atlas -sheet board:work:work-long-plan` (or
// `work-long-result`). `-plan-bodies-missing` leaves the bodies out (the
// page shows the previews and "Loading the full plan…");
// `-plan-bodies-late <secs>` writes them after that many seconds.

import Foundation
import Loro

extension DemoDataset {
    static let longPlanWorkId = "work-long-plan"
    static let longResultWorkId = "work-long-result"

    /// Adds the two work items, and their bodies unless `bodies` is false.
    func addLongPlans(bodies: Bool = true) {
        let now = nowMs()
        let minute: Int64 = 60_000
        let instruction = Self.longPlanInstruction
        let details = Self.longPlanDetails
        let previews = Self.previews(details.map(\.1), budget: 4_000)
        let plan = PlanBody(
            rev: "5c1e9a03d27b4f68", instruction: instruction,
            details: Dictionary(uniqueKeysWithValues: details.map { ($0.0, $0.1) }), at: now - 6 * minute)
        let longPlan = BotWork(
            id: Self.longPlanWorkId, botId: Self.atlasId, title: "Move the invoice service onto the ledger API",
            instruction: clip(instruction, max: BotCaps.instruction),
            summary: "Map every call, switch them over behind a flag, then prove the totals match.",
            steps: zip(details, previews).map { pair, preview in
                WorkStep(
                    id: pair.0, title: Self.longPlanTitles[pair.0] ?? pair.0, project: "space-cypher", detail: preview)
            },
            state: .proposed, chatId: "chat-atlas", toolCallId: "call-plan-long",
            createdAt: now - 6 * minute, bodyRev: plan.rev)

        let result = PlanResult(rev: "a7f20c94e1b35d80", text: Self.longResult, at: now - 50 * minute)
        let longResult = BotWork(
            id: Self.longResultWorkId, botId: Self.atlasId, title: "Audit the error messages in the CLI",
            instruction: "Go through every error the CLI can print and rewrite the unclear ones.",
            summary: "List every error path, then rewrite each that doesn't say what to do next.",
            steps: [
                WorkStep(
                    id: "1", title: "List every error the CLI prints", project: "space-cypher", state: .done,
                    startedAt: now - 80 * minute, endedAt: now - 65 * minute),
                WorkStep(
                    id: "2", title: "Rewrite the unclear ones", project: "space-cypher", state: .done,
                    startedAt: now - 65 * minute, endedAt: now - 50 * minute),
            ],
            state: .done, chatId: "chat-atlas", toolCallId: "call-plan-audit",
            createdAt: now - 85 * minute, startedAt: now - 80 * minute, endedAt: now - 50 * minute,
            result: clip(Self.longResult, max: BotCaps.result),
            decision: PlanDecision(approve: true, at: now - 82 * minute), resultRev: result.rev)
        botRecords.work += [longPlan, longResult]
        if bodies { writePlanBodies() }
    }

    /// Writes the two bodies into Atlas's chat doc, as its host would.
    func writePlanBodies() {
        let store = sessionStore(for: "chat-atlas")
        let map = store.doc.getMap(id: "planBodies")
        let now = nowMs()
        let plan = PlanBody(
            rev: "5c1e9a03d27b4f68", instruction: Self.longPlanInstruction,
            details: Dictionary(uniqueKeysWithValues: Self.longPlanDetails.map { ($0.0, $0.1) }), at: now)
        let result = PlanResult(rev: "a7f20c94e1b35d80", text: Self.longResult, at: now)
        func put<T: Encodable>(_ workId: String, _ part: String, _ value: T) {
            guard let data = try? JSONEncoder().encode(value),
                let json = try? JSONSerialization.jsonObject(with: data),
                let entry = try? map.insertContainer(key: workId, child: LoroMap())
            else { return }
            try? entry.insert(key: part, v: LoroValue.fromJSON(json))
        }
        put(Self.longPlanWorkId, "plan", plan)
        put(Self.longResultWorkId, "result", result)
        store.doc.commit()
        store.refreshPlanBodies()
    }

    /// The row's previews of `details`, the way the host's clamp makes them:
    /// they share one budget, the longest giving way first.
    static func previews(_ details: [String], budget: Int) -> [String] {
        var out = details
        while out.reduce(0, { $0 + $1.count }) > budget,
            let longest = out.indices.max(by: { out[$0].count < out[$1].count })
        {
            let over = out.reduce(0) { $0 + $1.count } - budget
            out[longest] = clip(out[longest], max: max(out[longest].count - over, 1))
        }
        return out
    }

    private static let longPlanTitles = [
        "1": "Map every call to the old billing client",
        "2": "Add the ledger client behind a flag",
        "3": "Switch the calls over, one module at a time",
        "4": "Compare a month of totals, then remove the old path",
    ]

    private static let longPlanInstruction = """
        The invoice service still talks to the old billing client, and the ledger team wants it off \
        by the end of the month. Move it onto the ledger API.

        Some things I care about, in order:

        1. Nothing changes for customers. Totals, rounding, tax lines and the PDF layout must come \
        out exactly the same. If you find a place where the old client was wrong, write it down \
        and keep the old behaviour for now; we fix those separately.
        2. It has to be reversible for at least two weeks. Put the new path behind a flag that we \
        can flip per tenant, and keep the old code until we've run a full billing cycle on the new one.
        3. Don't touch the public API. The web app and the mobile apps call `/invoices` and \
        `/invoices/{id}/pdf`; their request and response shapes stay as they are.
        4. Retries. The old client retried everything three times, including the POSTs that create \
        charges, which is how we double-charged two tenants in March. The ledger API has \
        idempotency keys: use them, and only retry what is safe to retry.
        5. Logging. Every call to the ledger should log the tenant, the endpoint, the idempotency \
        key and how long it took, but never the amounts or the customer's name.
        6. Tests. The billing tests mock the old client's HTTP layer, so they will pass whatever \
        the ledger does. Add tests that run against the ledger sandbox's recorded responses, and \
        keep a handful of the old mocks so both paths stay covered while the flag exists.

        The ledger team's docs are in the shared drive under "Ledger API v3"; their sandbox is \
        `ledger-sandbox.internal` and the credentials are in the usual vault path. Ask me before \
        you do anything against production. When you're done I'd like a short write-up of what \
        changed, what you found that looked wrong, and how to flip the flag back.
        """

    private static let longPlanDetails: [(String, String)] = [
        (
            "1",
            """
            Read `services/invoice/src/billing/` and list every call into the old client, with the \
            module it is in, what it sends and what it expects back. From a first look there are \
            about twenty:

            \(oldCalls.map { "- `\($0.0)` in `\($0.1)`: \($0.2)" }.joined(separator: "\n"))

            For each one, note:

            - whether it reads or writes (writes need an idempotency key on the ledger side);
            - which fields of the response the service actually uses, so the mapping stays small;
            - how errors come back today (exceptions, status codes, or `null` for "not found"), and \
            which callers depend on that shape;
            - whether the old client retried it, and whether retrying it is safe.

            Write the list to a migration note as a table, one row per call, and mark the \
            ones that look wrong today (the March double charges came from `create_charge` being \
            retried without a key). This step changes no code.

            Three calls need a closer look before anything moves:

            - `render_statement` reads figures the ledger computes differently (it rounds per line, \
            the old client per invoice). Record a month of statements from both and diff them.
            - `export_month` is used by the accountant's spreadsheet, which expects the old column \
            order. Keep the CSV identical even if the ledger returns the columns in another order.
            - `record_payment` is called by hand from the admin page with free-text references. \
            Find out whether the ledger accepts references longer than 64 characters; the old \
            client silently cut them, and finance matches payments on them.

            If any call has no counterpart in the ledger API, stop and tell me which before step 2.

            The table should look like this, so the next steps can work through it row by row:

            | Call | Module | Reads/writes | Fields used | Errors today | Retried | Safe to retry |
            | --- | --- | --- | --- | --- | --- | --- |
            | `get_customer` | customers.rs | reads | name, address, tax id | `null` if unknown | 3× | yes |
            | `create_charge` | charges.rs | writes | id, status | exception | 3× | **no** (no key) |
            | `create_refund` | refunds.rs | writes | id, status, amount | status 409 on repeat | none | with a key |

            Read the callers too, not only the client: several modules catch the client's \
            exceptions and turn them into `None`, and the ledger client must keep that shape or \
            those callers start failing loudly. Search for `BillingError` and for every `unwrap_or` \
            after a billing call; list each such caller in a second table under the first, with the \
            line it is on. Note any call made inside a database transaction, since the ledger calls \
            take longer and the transaction timeouts (5 s) may need to move. Finally, check the \
            nightly jobs in `services/invoice/src/jobs/`: the dunning job and the month-end export \
            both call the client in loops over every tenant, and they are where rate limits will \
            bite first. Give each loop's call count for the largest tenant last month.
            """
        ),
        (
            "2",
            """
            Add `LedgerClient` in `services/invoice/src/ledger/` with one method per row of the \
            table from step 1. It sends an idempotency key on every write (derived from the invoice \
            id and the line, so a retry after a crash reuses it), retries only reads and keyed \
            writes, with backoff, and logs tenant, endpoint, key and duration — never amounts or \
            names.

            Add the flag `invoice.ledger_v3` (off by default, per tenant) and a `BillingBackend` \
            trait that both clients implement, so the callers only ever see the trait. Unit-test the \
            mapping of each response against the sandbox's recorded fixtures.
            """
        ),
        (
            "3",
            """
            Move the callers onto `BillingBackend`, one module at a time (charges, refunds, tax, \
            PDF), each in its own commit with its tests green against both backends. Keep the old \
            behaviour where step 1 found it wrong, with a `// ledger-migration:` comment pointing \
            at the table row.
            """
        ),
        (
            "4",
            """
            Replay last month's invoices for three tenants through both backends in the sandbox and \
            compare totals, tax lines and PDFs byte for byte. Report any difference before going on. \
            Only then flip the flag for the internal tenant; the old path is removed in a later PR, \
            after a full billing cycle.
            """
        ),
    ]

    private static let oldCalls: [(String, String, String)] = [
        ("get_customer", "customers.rs", "the billing profile, for the address block"),
        ("get_tax_rates", "tax.rs", "rates by region, cached for an hour"),
        ("create_charge", "charges.rs", "one charge per invoice line"),
        ("capture_charge", "charges.rs", "after the customer confirms"),
        ("void_charge", "charges.rs", "when an invoice is cancelled before capture"),
        ("list_charges", "charges.rs", "for the invoice history page"),
        ("create_refund", "refunds.rs", "full and partial refunds"),
        ("get_refund", "refunds.rs", "to poll a refund's state"),
        ("list_refunds", "refunds.rs", "per invoice"),
        ("get_balance", "balance.rs", "the tenant's prepaid balance"),
        ("apply_credit", "balance.rs", "credits from support"),
        ("get_currency", "currency.rs", "exchange rates for multi-currency tenants"),
        ("render_statement", "pdf.rs", "the monthly statement's figures"),
        ("get_payment_methods", "customers.rs", "cards on file, masked"),
        ("set_default_method", "customers.rs", "from the settings page"),
        ("create_invoice_record", "invoices.rs", "the ledger's copy of the invoice"),
        ("finalize_invoice_record", "invoices.rs", "locks it for edits"),
        ("get_dunning_state", "dunning.rs", "for overdue reminders"),
        ("record_payment", "payments.rs", "bank transfers matched by hand"),
        ("export_month", "exports.rs", "the accountant's CSV"),
    ]

    private static let longResult: String = {
        let rewrites = [
            ("`error: config`", "“Couldn't read ~/.cypher/config.toml: line 12 has no value after `model =`.”"),
            ("`error: io`", "“Couldn't write the log file: the disk is full.”"),
            ("`unauthorized`", "“Your sign-in expired. Run `cypher login` again.”"),
            ("`no such chat`", "“There is no chat named ‘release-notes’. `cypher chats` lists them.”"),
            ("`timeout`", "“The host didn't answer within 30 s. Is the Mac awake?”"),
            ("`bad flag`", "“`--model` needs a value, e.g. `--model claude-sonnet`.”"),
            ("`conflict`", "“Someone else changed this routine; reload it and try again.”"),
            ("`rate limited`", "“Too many requests; try again in a minute.”"),
            ("`invalid path`", "“That path is outside the project. Paths are relative to its folder.”"),
            ("`panic`", "“Something went wrong inside the CLI. The log is at …; please send it to us.”"),
        ]
        var text = """
            Rewrote **38 of the CLI's 112 error messages**; the other 74 already said what went \
            wrong and what to do. Every rewrite is in `crates/cli/src/errors.rs`, and \
            `cargo test -p cypher-cli` passes.

            ## What changed

            Each rewritten message now says what failed, in the user's words, and the next thing \
            to try. The ones that changed most:

            """
        for (old, new) in rewrites {
            text += "- \(old) → \(new)\n"
        }
        text += """

            ## Patterns I fixed everywhere

            - **Error kinds as messages.** Twenty-one messages were just the error's kind \
            (`io`, `config`, `serde`). They now name the file or the setting.
            - **Paths.** Messages print the path the user typed, not the canonical one, and \
            shorten the home folder to `~`.
            - **Next steps.** Where a command can fix it (`cypher login`, `cypher doctor`), the \
            message names that command.
            - **No blame.** “You provided an invalid …” became “… isn't a …”.

            ## What I left alone

            - The 14 messages that come straight from the Pi runtime. They are not ours to \
            change here; I listed them in the PR so the runtime can follow.
            - Exit codes. Two scripts in CI match on them, so none changed.

            ## Worth a look

            Three messages could not be made clear without changing behaviour:

            1. `cypher sync` prints “partial” when some chats didn't sync, without saying which. \
            It needs the list from the engine, which the RPC doesn't return yet.
            2. `cypher run --detach` fails silently when the host is asleep; it should wait or say so.
            3. The config loader stops at the first bad line. Showing every bad line at once \
            would save a round trip, but it changes the loader.

            """
        for round in 1...6 {
            text += """

                ### Round \(round) of checks

                Ran every command with a broken config, a missing network, an expired sign-in and a \
                read-only home folder, and read each message as a new user would. Round \(round) \
                found \(7 - round) message\(round == 6 ? "" : "s") still unclear; each is fixed in \
                the commit named after the round.

                """
        }
        return text
    }()
}
