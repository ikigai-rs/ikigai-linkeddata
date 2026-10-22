//! A standalone consumer: workspace dev-dependencies must not unify formats back on.
#[no_mangle]
pub extern "C" fn check_depth(levels: u32) -> bool {
    let text = format!(
        "{}<urn:s> <urn:p> <urn:o>{}",
        "<<(".repeat(levels as usize),
        ")>>".repeat(levels as usize)
    );
    ikigai_rdf::check_turtle_nesting(&text, "content").is_ok()
}

#[no_mangle]
pub extern "C" fn subclass_count() -> usize {
    ikigai_rdf::subclass_axioms(
        "<urn:a> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:b> .",
    )
    .len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turtle_boundary_and_parser_work_without_formats() {
        assert!(check_depth(64));
        assert!(!check_depth(65));
        assert_eq!(subclass_count(), 1);
        assert!(ikigai_rdf::check_json_nesting(b"{\"key\": [1]}", "content").is_ok());
    }

    #[cfg(feature = "rdfxml")]
    #[test]
    fn xml_scan_works_without_oxrdfio() {
        let xml = format!(
            "{}<end/>{}",
            "<p parseType=\"Triple\">".repeat(65),
            "</p>".repeat(65)
        );
        assert!(ikigai_rdf::check_rdfxml_nesting(xml.as_bytes(), "content").is_err());
    }

    #[cfg(feature = "full")]
    #[test]
    fn full_api_remains_available() {
        let _ = ikigai_rdf::space();
        assert_eq!(ikigai_rdf::SPACE_ID, "urn:iki:space:rdf");
    }
}
