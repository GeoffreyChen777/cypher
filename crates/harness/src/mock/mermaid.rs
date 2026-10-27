//! Mermaid demo reply for the mock harness: the diagram families a model
//! reaches for, a CJK one, one wider than the column, one nested in a list
//! and one that does not parse — streamed in small chunks, so each fence is
//! seen as code until its closing line arrives.

use cypher_proto::AgentEvent;

/// Prompt marker: reply with the Mermaid showcase.
pub const MARKER: &str = "[mock:mermaid]";

const REPLY: &str = r##"Here is how a prompt travels through the app:

```mermaid
flowchart TD
    A[User sends a prompt] --> B{Needs tools?}
    B -->|yes| C[Run tool calls]
    B -->|no| D[Stream the reply]
    C --> E[(Session store)]
    C --> D
    D --> F([Done])
```

The same exchange as a sequence:

```mermaid
sequenceDiagram
    participant UI
    participant Engine
    participant Model
    UI->>Engine: send(prompt)
    Engine->>Model: complete
    Model-->>Engine: stream deltas
    Note over Engine,Model: tool loop
    Engine-->>UI: transcript update
```

A session's lifecycle:

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Streaming: send
    Streaming --> Tools: tool call
    Tools --> Streaming: result
    Streaming --> Idle: done
    Streaming --> Error: failure
    Error --> [*]
```

Where the tokens went, and the pieces of the app:

```mermaid
pie title Tokens per turn
    "Prompt" : 42
    "Tool output" : 30
    "Reply" : 28
```

```mermaid
mindmap
  root((Cypher))
    UI
      Transcript
      Composer
    Engine
      Sessions
      Tools
```

```mermaid
flowchart LR
    用户[用户] --> 引擎[引擎处理请求] --> 模型[模型生成回复]
```

A pipeline wider than the column scrolls sideways:

```mermaid
flowchart LR
    Fetch[Fetch sources] --> Parse[Parse markdown] --> Layout[Lay out blocks] --> Highlight[Highlight code] --> Typeset[Typeset math] --> Diagram[Draw diagrams] --> Paint[Paint the frame] --> Present[Present to screen]
```

1. Diagrams work inside lists too:

   ```mermaid
   classDiagram
       class Transcript {
           +rows: Vec~Row~
           +sync()
       }
       class Row {
           +id: String
           +version: u64
       }
       Transcript "1" --> "*" Row
   ```

2. A diagram that does not parse stays code, with the reason:

```mermaid
flowchart TD
    A --> 
```
"##;

/// Bytes per streamed delta: small enough that every fence streams in over
/// several commits.
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
        assert_eq!(text.matches("```mermaid").count(), 9);
    }
}
