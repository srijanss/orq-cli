//! A frontend feature's sessions (implement, design fix, gate fix) are told its tests cover logic and
//! behaviour a screenshot cannot show, not how the page looks. A standard feature's prompt is unchanged.

use orq_agents::{adapter_for, FixRequest, ImplementRequest, ModelSpec};

/// Words of each of the rule's three parts.
const RULE: [&str; 3] = [
    "Test: views, context and Python/JS logic",
    "Don't test: layout, spacing, sizes, colours, fonts, breakpoints or markup structure",
    "Browser tests only when nothing else can check it",
];

fn spec(harness: &str, model: &str) -> ModelSpec {
    ModelSpec { harness: harness.to_string(), model: model.to_string(), thinking: None }
}

fn request(implementer: &str, frontend: bool) -> ImplementRequest<'static> {
    ImplementRequest {
        features: &["home-v02-page-order"],
        implementer: spec(implementer, "opus"),
        reviewer: spec("pi", "openai/gpt-6-astra"),
        context: None,
        exit_marker: Some("/logs/implement.done"),
        attended: false,
        unattended: true,
        remote_control: None,
        handoff: None,
        continuing: None,
        review_again: None,
        leave_full_suite_to_gate: true,
        allowed_commands: &[],
        declared_files: &[],
        answer: None,
        undeclared_files: &[],
        reopened_findings: &[],
        feature_specs: &[],
        frontend,
    }
}

fn fix(harness: &str, task: &'static str, frontend: bool) -> FixRequest<'static> {
    FixRequest { implementer: spec(harness, "opus"), task, exit_marker: Some("/logs/fix.done"), attended: false, allowed_commands: &[], frontend }
}

fn has_rule(text: &str) -> bool {
    RULE.iter().all(|part| text.contains(part))
}

#[test]
fn a_frontend_requests_implement_argv_has_the_rule_and_a_standard_ones_does_not() {
    for harness in ["claude", "pi"] {
        let adapter = adapter_for(harness).unwrap();
        let frontend = adapter.implement_argv(&request(harness, true)).unwrap();
        let prompt = frontend.last().unwrap();
        assert!(has_rule(prompt), "{harness}: {prompt}");
        assert!(prompt.contains("The design review checks these from captures."), "{harness}: {prompt}");
        assert!(prompt.contains("Prefer Django test-client or template-render tests."), "{harness}: {prompt}");

        let standard = adapter.implement_argv(&request(harness, false)).unwrap();
        assert!(!standard.iter().any(|a| RULE.iter().any(|part| a.contains(part))), "{harness}: {standard:?}");
        // The rest of a standard feature's prompt is what it was.
        assert_eq!(standard.last().unwrap().len() + frontend_extra(prompt, standard.last().unwrap()), prompt.len());
    }
}

/// How much longer the frontend prompt is: exactly the rule's paragraph, nothing else differs.
fn frontend_extra(frontend: &str, standard: &str) -> usize {
    let start = frontend.find(RULE[0]).unwrap();
    let common_prefix = frontend.bytes().zip(standard.bytes()).take_while(|(a, b)| a == b).count();
    assert!(common_prefix <= start, "the rule is where the prompts first differ");
    let extra = frontend.len() - standard.len();
    assert_eq!(&frontend[..common_prefix], &standard[..common_prefix]);
    assert_eq!(&frontend[common_prefix + extra..], &standard[common_prefix..], "only the rule was added");
    extra
}

#[test]
fn a_design_fix_and_a_gate_fix_prompt_for_a_frontend_member_have_the_rule() {
    for harness in ["claude", "pi"] {
        let adapter = adapter_for(harness).unwrap();
        for task in ["Design correction round 1: fix ISSUE-1, the hero title overflows.", "The local gate failed after your changes. Fix it."] {
            let frontend = adapter.fix_argv(&fix(harness, task, true)).unwrap();
            assert!(has_rule(frontend.last().unwrap()), "{harness} {task}: {frontend:?}");
            let standard = adapter.fix_argv(&fix(harness, task, false)).unwrap();
            assert!(!standard.iter().any(|a| RULE.iter().any(|part| a.contains(part))), "{harness} {task}: {standard:?}");
        }
    }
}
