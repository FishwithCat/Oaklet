use regex_lite::Regex;

#[test]
fn start_is_a_byte_offset_in_the_original_haystack() {
    let re = Regex::new(r"\w+").unwrap();
    let hay = "é one two three";
    let ranges: Vec<_> = re.find_iter_at(hay, 7).map(|m| m.range()).collect();
    assert_eq!(ranges, [7..10, 11..16]);
    assert_eq!(
        re.find_iter_at(hay, 0)
            .map(|m| m.range())
            .collect::<Vec<_>>(),
        re.find_iter(hay).map(|m| m.range()).collect::<Vec<_>>()
    );
    assert!(re.find_iter_at(hay, hay.len() + 1).next().is_none());
}

#[test]
fn assertions_see_prefix_and_empty_matches_advance() {
    assert!(
        Regex::new("^word")
            .unwrap()
            .find_iter_at("xword", 1)
            .next()
            .is_none()
    );
    assert!(
        Regex::new(r"\bword")
            .unwrap()
            .find_iter_at("xword", 1)
            .next()
            .is_none()
    );
    let ranges: Vec<_> = Regex::new("")
        .unwrap()
        .find_iter_at("éé", 2)
        .map(|m| m.range())
        .collect();
    assert_eq!(ranges, [2..2, 4..4]);
}
