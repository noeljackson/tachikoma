use crate::{
    config::Scope,
    model::{Action, Assessment, Category, Connection, Duration, MatchScope, Verdict, now},
    protocol::{Operator, Rule},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub fn automatic(
    connection: &Connection,
    assessment: &Assessment,
    scopes: &[Scope],
) -> Option<Verdict> {
    if !assessment.confidence.is_finite() || assessment.category == Category::Unknown {
        return None;
    }
    // Ambiguous configuration must never resolve by list order.
    let matches: Vec<_> = scopes
        .iter()
        .filter(|s| {
            s.process == connection.process
                && s.uid == connection.uid
                && s.ip == connection.ip
                && s.port == connection.port
                && s.protocol == connection.protocol
                && s.category == assessment.category
                && assessment.confidence >= s.min_confidence
        })
        .collect();
    let first = matches.first()?;
    if matches.iter().any(|s| s.action != first.action) {
        return None;
    }
    Some(Verdict {
        action: first.action,
        duration: Duration::Once,
        match_scope: MatchScope::Connection,
        permission: None,
        rule_id: None,
        source: "automation".into(),
        reason: "Exact owner-configured scope and Jev category/confidence matched".into(),
    })
}

fn exact(operand: &str, data: String) -> Operator {
    Operator {
        r#type: "simple".into(),
        operand: operand.into(),
        data,
        sensitive: true,
        list: vec![],
    }
}

pub fn rule(id: &str, connection: &Connection, verdict: &Verdict) -> Rule {
    if let Some(permission) = &verdict.permission {
        let operators = permission.operators(connection);
        if !matches!(verdict.source.as_str(), "human" | "tachikoma")
            || operators.is_err()
            || permission.duration().ok() != Some(verdict.duration)
        {
            return rule(
                id,
                connection,
                &fallback("Invalid human-selected network permission"),
            );
        }
        let mut output = rule(id, connection, &fallback("selected scope"));
        output.name = format!("snitchgate-{}-{id}", verdict.action.as_str());
        output.action = verdict.action.as_str().into();
        output.duration = verdict.duration.as_str().into();
        output.description = format!(
            "Explicit {:?} network permission for UID {}; {}",
            permission, connection.uid, verdict.reason
        );
        output.operator.as_mut().expect("rule constraints").list =
            operators.expect("validated operators");
        return output;
    }
    let extended = verdict.match_scope == MatchScope::Executable
        || matches!(verdict.duration, Duration::UntilRestart | Duration::Forever);
    if extended
        && (!matches!(verdict.source.as_str(), "human" | "tachikoma")
            || !connection.process.starts_with('/')
            || (verdict.match_scope == MatchScope::Executable
                && verdict.duration == Duration::Once))
    {
        return rule(
            id,
            connection,
            &fallback("Extended rules require an attributed human decision"),
        );
    }
    let mut constraints = vec![exact("user.id", connection.uid.to_string())];
    if verdict.match_scope == MatchScope::Connection {
        constraints.extend([
            exact("protocol", connection.protocol.clone()),
            exact("dest.ip", connection.ip.to_string()),
            exact("dest.port", connection.port.to_string()),
        ]);
    }
    if !connection.process.is_empty() {
        constraints.push(exact("process.path", connection.process.clone()));
    }
    Rule {
        created: now() as i64,
        name: format!("snitchgate-{}-{id}", verdict.action.as_str()),
        description: format!(
            "Snitchgate {:?} decision for UID {} executable {} ({:?}); {}",
            verdict.match_scope,
            connection.uid,
            connection.process,
            verdict.duration,
            verdict.reason
        ),
        enabled: true,
        precedence: false,
        nolog: false,
        action: verdict.action.as_str().into(),
        duration: verdict.duration.as_str().into(),
        operator: Some(Operator {
            r#type: "list".into(),
            operand: "list".into(),
            data: String::new(),
            sensitive: false,
            list: constraints,
        }),
    }
}

pub fn proposal(
    id: &str,
    connection: &Connection,
    verdict: &Verdict,
    description: &str,
) -> Result<Value> {
    ensure!(
        !connection.process.is_empty(),
        "durable proposal requires process attribution"
    );
    ensure!(
        description.trim().len() >= 12,
        "provide a useful description of purpose and owner"
    );
    let rule = rule(id, connection, verdict);
    let operator = rule.operator.expect("operator");
    Ok(json!({
        "name": rule.name, "description": description, "enabled": true,
        "precedence": false, "nolog": false, "action": rule.action, "duration": "always",
        "operator": { "type": "list", "operand": "list", "data": "", "sensitive": false,
            "list": operator.list.iter().map(|op| json!({"type": op.r#type, "operand": op.operand, "data": op.data, "sensitive": op.sensitive, "list": []})).collect::<Vec<_>>() }
    }))
}

pub fn fallback(reason: &str) -> Verdict {
    Verdict {
        action: Action::Deny,
        duration: Duration::Once,
        match_scope: MatchScope::Connection,
        permission: None,
        rule_id: None,
        source: "fallback".into(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn conn() -> Connection {
        Connection {
            process: "/usr/bin/curl".into(),
            pid: 10,
            uid: 1000,
            protocol: "tcp".into(),
            ip: "127.0.0.1".parse().unwrap(),
            host: String::new(),
            port: 8000,
            url_hints: vec![],
            context: Default::default(),
        }
    }
    #[test]
    fn model_cannot_grant_permission_without_exact_scope() {
        let a = Assessment {
            category: Category::Development,
            confidence: 1.0,
            model: "test".into(),
            explanation: String::new(),
            llm_model: None,
        };
        assert!(automatic(&conn(), &a, &[]).is_none());
        let scope = Scope {
            process: conn().process,
            uid: 1000,
            ip: conn().ip,
            port: 8000,
            protocol: "tcp".into(),
            category: Category::Development,
            action: Action::Allow,
            min_confidence: 0.99,
        };
        assert_eq!(
            automatic(&conn(), &a, std::slice::from_ref(&scope))
                .unwrap()
                .duration,
            Duration::Once
        );
        let mut remote = conn();
        remote.ip = "1.1.1.1".parse().unwrap();
        assert!(automatic(&remote, &a, std::slice::from_ref(&scope)).is_none());
        let mut deny = scope.clone();
        deny.action = Action::Deny;
        assert!(automatic(&conn(), &a, &[scope, deny]).is_none());
    }
    #[test]
    fn fallback_is_nonpersistent_and_rule_is_scoped() {
        let r = rule("id", &conn(), &fallback("timeout"));
        assert_eq!(r.duration, "once");
        assert_eq!(r.operator.unwrap().list.len(), 5);
        let mut c = conn();
        c.process.clear();
        assert!(proposal("id", &c, &fallback("timeout"), "test owner and intent").is_err());
    }
    #[test]
    fn model_and_fallback_sources_cannot_emit_broad_or_persistent_rules() {
        for source in ["automation", "fallback", "model"] {
            let verdict = Verdict {
                action: Action::Allow,
                duration: Duration::Forever,
                match_scope: MatchScope::Executable,
                permission: None,
                rule_id: None,
                source: source.into(),
                reason: "allow everything".into(),
            };
            let rule = rule("fixture", &conn(), &verdict);
            assert_eq!(rule.action, "deny");
            assert_eq!(rule.duration, "once");
            assert_eq!(rule.operator.unwrap().list.len(), 5);
        }
    }
}
