use serde::Serialize;
use serde_json::Value;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    #[serde(rename = "type")]
    pub relation_type: &'static str,
    pub target_ref: String,
}

#[derive(Debug)]
pub struct InvalidReference;

fn valid_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 63
        && part.as_bytes()[0].is_ascii_alphanumeric()
        && part.as_bytes()[part.len() - 1].is_ascii_alphanumeric()
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn canonical_reference(
    reference: &str,
    default_kind: &str,
    default_namespace: &str,
    allowed_kinds: &[&str],
) -> Result<String, InvalidReference> {
    let (kind, rest) = reference
        .split_once(':')
        .unwrap_or((default_kind, reference));
    let (namespace, name) = rest.split_once('/').unwrap_or((default_namespace, rest));
    let kind = kind.to_ascii_lowercase();
    if !allowed_kinds.contains(&kind.as_str()) || !valid_part(namespace) || !valid_part(name) {
        return Err(InvalidReference);
    }
    Ok(format!(
        "{kind}:{}/{}",
        namespace.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    ))
}

struct Field {
    name: &'static str,
    relation_type: &'static str,
    kinds: &'static [&'static str],
    multiple: bool,
}

const FIELDS: &[Field] = &[
    Field {
        name: "owner",
        relation_type: "ownedBy",
        kinds: &["group", "user"],
        multiple: false,
    },
    Field {
        name: "system",
        relation_type: "partOf",
        kinds: &["system"],
        multiple: false,
    },
    Field {
        name: "subcomponentOf",
        relation_type: "partOf",
        kinds: &["component"],
        multiple: false,
    },
    Field {
        name: "dependsOn",
        relation_type: "dependsOn",
        kinds: &["component", "resource"],
        multiple: true,
    },
    Field {
        name: "providesApis",
        relation_type: "providesApi",
        kinds: &["api"],
        multiple: true,
    },
    Field {
        name: "consumesApis",
        relation_type: "consumesApi",
        kinds: &["api"],
        multiple: true,
    },
];

pub fn project(spec: &Value, namespace: &str) -> Result<Vec<Relation>, InvalidReference> {
    let mut relations = Vec::new();
    for field in FIELDS {
        let Some(value) = spec.get(field.name) else {
            if field.name == "owner" {
                return Err(InvalidReference);
            }
            continue;
        };
        let references = if field.multiple {
            value.as_array().ok_or(InvalidReference)?.as_slice()
        } else {
            std::slice::from_ref(value)
        };
        for reference in references {
            let target_ref = canonical_reference(
                reference.as_str().ok_or(InvalidReference)?,
                field.kinds[0],
                namespace,
                field.kinds,
            )?;
            relations.push(Relation {
                relation_type: field.relation_type,
                target_ref,
            });
        }
    }
    // The inverse field is validated but not exposed as an outbound relation in this subset.
    if let Some(value) = spec.get("dependencyOf") {
        for reference in value.as_array().ok_or(InvalidReference)? {
            canonical_reference(
                reference.as_str().ok_or(InvalidReference)?,
                "component",
                namespace,
                &["component", "resource"],
            )?;
        }
    }
    Ok(relations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonicalizes_explicit_and_implicit_reference_parts() {
        for (input, expected) in [
            ("Team-A", "group:platform/team-a"),
            ("Other/Team-A", "group:other/team-a"),
            ("User:Other/Alice", "user:other/alice"),
            ("user:Alice", "user:platform/alice"),
        ] {
            assert_eq!(
                canonical_reference(input, "group", "platform", &["group", "user"]).unwrap(),
                expected
            );
        }
        for input in [
            "",
            ":name",
            "group:/name",
            "group:ns/",
            "group:ns/a/b",
            "group:ns/a:b",
            "../name",
            "https://host/name",
            "group:ns/a b",
            "component:name",
        ] {
            assert!(
                canonical_reference(input, "group", "platform", &["group", "user"]).is_err(),
                "{input}"
            );
        }
    }

    #[test]
    fn projects_supported_fields_without_resolving_targets() {
        let spec = json!({"owner":"team", "system":"payments", "subcomponentOf":"parent", "dependsOn":["database", "resource:shared/cache"], "providesApis":["v1"], "consumesApis":["api:external/remote"]});
        let actual = serde_json::to_value(project(&spec, "platform").unwrap()).unwrap();
        assert_eq!(
            actual,
            json!([
                {"type":"ownedBy","targetRef":"group:platform/team"},
                {"type":"partOf","targetRef":"system:platform/payments"},
                {"type":"partOf","targetRef":"component:platform/parent"},
                {"type":"dependsOn","targetRef":"component:platform/database"},
                {"type":"dependsOn","targetRef":"resource:shared/cache"},
                {"type":"providesApi","targetRef":"api:platform/v1"},
                {"type":"consumesApi","targetRef":"api:external/remote"}
            ])
        );
        assert!(
            project(
                &json!({"owner":"team", "dependsOn":["group:team"]}),
                "platform"
            )
            .is_err()
        );
        assert!(project(&json!({"owner":"team", "providesApis":"api"}), "platform").is_err());
    }
}
