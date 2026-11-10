// A workspace's routines: the list in order of next run, one routine (where
// and when it runs, its prompt, the last run; Run now, Edit, Delete) and the
// routine form. Stubs.

import SwiftUI

struct RoutineListPage: View {
    @Environment(AppModel.self) private var model
    let botId: String

    var body: some View {
        BotStubPage(title: "Routines") {
            ForEach(model.data?.routines(of: botId) ?? []) { routine in
                NavigationLink(value: BotPage.routine(routine.id)) {
                    LabeledContent(routine.name, value: routine.schedule.describe)
                }
            }
        }
    }
}

struct RoutinePage: View {
    @Environment(AppModel.self) private var model
    let botId: String
    let routineId: String

    var body: some View {
        let routine = model.data?.routine(id: routineId)
        BotStubPage(title: routine?.name ?? "Routine") {
            if let routine {
                BotFact(label: "Schedule", value: routine.schedule.describe)
                if let run = routine.lastRun {
                    BotFact(label: "Last run", status: run.outcome.status)
                }
                NavigationLink("Edit", value: BotPage.routineForm(routineId: routine.id))
            }
        }
    }
}

struct RoutineFormPage: View {
    let botId: String
    /// nil: a new routine.
    let routineId: String?

    var body: some View {
        BotStubPage(title: routineId == nil ? "New routine" : "Edit routine") {
            Text("Routine form")
        }
    }
}
