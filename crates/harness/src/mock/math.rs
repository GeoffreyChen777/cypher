//! Math demo reply for the mock harness: the shapes of LaTeX a model writes
//! in chat — `$…$` and `\(…\)` inline, `$$…$$` and `\[…\]` display, prices
//! that must stay prose, and TeX that does not typeset — so the transcript's
//! formula rendering can be seen without a real model.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the math showcase.
pub const MARKER: &str = "[mock:math]";

const REPLY: &str = r##"Here is the quadratic formula. For $ax^2 + bx + c = 0$ with $a \neq 0$, the roots are

$$
x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}
$$

and the discriminant \(\Delta = b^2 - 4ac\) decides how many are real: two when $\Delta > 0$, one when $\Delta = 0$.

**Euler's identity** links the five constants $e$, $i$, $\pi$, $1$ and $0$:
\[
e^{i\pi} + 1 = 0
\]

A few more shapes, inline: $\sum_{k=1}^{n} k = \frac{n(n+1)}{2}$, $\int_0^1 x^2\,dx = \tfrac{1}{3}$, $\lim_{x \to 0} \frac{\sin x}{x} = 1$, and a matrix:

$$
A = \begin{pmatrix} \cos\theta & -\sin\theta \\ \sin\theta & \cos\theta \end{pmatrix}, \qquad \det A = 1
$$

| Quantity | Formula |
| --- | --- |
| Gaussian | $\frac{1}{\sigma\sqrt{2\pi}} e^{-\frac{(x-\mu)^2}{2\sigma^2}}$ |
| Big-O | $O(n \log n)$ |

- Prices stay prose: it costs $5 today and $10 tomorrow.
- Code keeps its dollars: `echo $HOME` and `\(not math\)`.
- TeX that does not typeset shows as code: $\undefinedmacro{x}$.
"##;

/// Bytes per streamed delta: small enough that formulas arrive half-written,
/// which is what the streaming path has to keep steady.
const CHUNK: usize = 24;

/// The showcase reply's text deltas.
pub(super) fn reply() -> Vec<AgentEvent> {
    let mut deltas = Vec::new();
    let mut start = 0;
    while start < REPLY.len() {
        let mut end = (start + CHUNK).min(REPLY.len());
        while !REPLY.is_char_boundary(end) {
            end += 1;
        }
        deltas.push(AgentEvent::TextDelta {
            text: REPLY[start..end].into(),
        });
        start = end;
    }
    deltas
}

#[cfg(test)]
mod tests {
    use cypher_proto::AgentEvent;

    #[test]
    fn deltas_reassemble_the_reply() {
        let text: String = super::reply()
            .into_iter()
            .map(|event| match event {
                AgentEvent::TextDelta { text } => text,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(text, super::REPLY);
        assert_eq!(text.matches("$$").count() % 2, 0);
    }
}
