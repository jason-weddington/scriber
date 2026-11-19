//! Embedder term normalization and wire forms.
#[cfg(test)]
mod app_tests {
    use scriber_core::app_terms::{AppTerm, MAX_CHARS, MAX_TERMS, PROMPT_SHARE, normalize};

    fn app(term: &str, spoken: Option<&str>) -> AppTerm {
        AppTerm {
            term: term.into(),
            spoken: spoken.map(str::to_string),
        }
    }

    #[test]
    fn limits_and_json_are_pinned() {
        assert_eq!((MAX_TERMS, MAX_CHARS, PROMPT_SHARE), (50, 64, 200));
        for text in [
            r#"{"term":"Zed"}"#,
            r#"{"term":"tightbeam-lead","spoken":"tightbeam lead"}"#,
        ] {
            let term: AppTerm = serde_json::from_str(text).unwrap();
            assert_eq!(serde_json::to_string(&term).unwrap(), text);
        }
        assert_eq!(
            serde_json::from_str::<AppTerm>(r#"{"term":"Zed"}"#)
                .unwrap()
                .spoken,
            None
        );
    }

    #[test]
    fn normalization_cleans_drops_and_preserves_priority() {
        assert_eq!(normalize(vec![]), []);
        let long = "é".repeat(64);
        let over = "é".repeat(65);
        assert_eq!(
            normalize(vec![
                app("", None),
                app(" \t ", None),
                app(" a\nb ", Some(" a\rb c ")),
                app(&long, None),
                app(&over, None),
                app("Zed", Some("zed aloud")),
                app("zed", Some("other")),
                app("term (x)", Some("say x"))
            ]),
            vec![
                app("ab", Some("ab c")),
                app(&long, None),
                app("Zed", Some("zed aloud")),
                app("term (x)", Some("say x"))
            ]
        );
        assert_eq!(
            normalize(vec![app("Zed", None), app("zed", Some("other"))]),
            vec![app("Zed", None)]
        );
        let many: Vec<_> = (0..51).map(|i| app(&format!("term{i}"), None)).collect();
        assert_eq!(normalize(many.clone()), many[..50]);
    }

    #[test]
    fn spoken_forms_have_independent_limits_and_no_parentheses() {
        for spoken in ["", "  ", "ZED", "tight (beam)", "tight)beam", "(beam"] {
            assert_eq!(
                normalize(vec![app("Zed", Some(spoken))]),
                vec![app("Zed", None)],
                "{spoken}"
            );
        }
        assert_eq!(
            normalize(vec![app("Zed", Some(&"é".repeat(65)))]),
            vec![app("Zed", None)]
        );
        assert_eq!(
            normalize(vec![app("Zed", Some(&"é".repeat(64)))]),
            vec![app("Zed", Some(&"é".repeat(64)))]
        );
    }
}
