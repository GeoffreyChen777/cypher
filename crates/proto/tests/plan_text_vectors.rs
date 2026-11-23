//! `PlanText::of` against `protocol/vectors/plan-text-v1.json`, which the
//! iPhone's twin (`apps/ios/Cypher/Bots/PlanText.swift`, `PlanTextTests`) runs
//! too. A case's `bodies` is the `planBodies[workId]` entry as the doc holds
//! it (`null`: no entry); each part is read on its own, as `cypher_doc`'s
//! `SessionDoc::plan_body` does, so a part that doesn't read is missing.

use cypher_proto::{BotWork, PlanText, TextSource, WorkBodies};
use serde_json::Value;

fn entry(value: &Value) -> WorkBodies {
    let part = |key: &str| value.get(key).cloned();
    WorkBodies {
        plan: part("plan").and_then(|plan| serde_json::from_value(plan).ok()),
        result: part("result").and_then(|result| serde_json::from_value(result).ok()),
    }
}

fn source(name: &str) -> TextSource {
    match name {
        "row" => TextSource::Row,
        "body" => TextSource::Body,
        "preview" => TextSource::Preview,
        other => panic!("unknown source {other:?}"),
    }
}

#[test]
fn plan_text_vectors() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../protocol/vectors/plan-text-v1.json")).unwrap();
    let cases = vectors["planText"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let work: BotWork = serde_json::from_value(case["work"].clone()).unwrap();
        let bodies = Some(&case["bodies"])
            .filter(|bodies| !bodies.is_null())
            .map(entry);
        let text = PlanText::of(&work, bodies.as_ref());
        let expect = &case["expect"];
        let details: Vec<Option<String>> =
            serde_json::from_value(expect["details"].clone()).unwrap();
        assert_eq!(
            text.instruction, expect["instruction"],
            "{name}: instruction"
        );
        assert_eq!(text.details, details, "{name}: details");
        assert_eq!(
            text.plan,
            source(expect["plan"].as_str().unwrap()),
            "{name}: plan"
        );
        assert_eq!(
            text.result.as_deref(),
            expect["result"].as_str(),
            "{name}: result"
        );
        assert_eq!(
            text.result_source,
            source(expect["resultSource"].as_str().unwrap()),
            "{name}: result source"
        );
        assert_eq!(text.is_loading(), expect["loading"], "{name}: loading");
    }
}
