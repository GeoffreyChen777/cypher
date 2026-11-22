# Next release (draft, unreleased)

Draft lines for the next desktop release, gathered as features land. The
version and the title are set when it is cut; move these into that version's
file then and delete this one.

## Workspaces

- **Plans can be longer.** A plan's step details may add up to 30,000
  characters (was 4,000 in all), and a plan's result to 30,000. The card and
  the plan's list keep a short preview; the plan's page shows the full text,
  with "Loading the full plan…" for a moment while it arrives. A plan over the
  limit is refused with how much it is over, so the agent shortens or splits
  it; nothing is cut without a word.
- **Plans can be longer once the device hosting the workspace runs the new
  version.** Until then its plans keep the old limits. Apps that are not
  updated still show the preview of a long plan, marked as cut.
- **Long texts are refused, not cut.** A routine's prompt or a shortcut's
  request over 4,000 characters, and your edit of an answer to review over
  8,000, used to be cut short silently. Now the routine form, the review
  panel and the agent's tools say how much it is over and keep what you
  wrote.
