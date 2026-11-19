use regex_syntax::ast::parse::Parser;

#[test]
fn parser_reuses_capture_and_flag_state() {
    let mut parser = Parser::new();
    assert!(parser.parse("a+").is_ok());
    let pattern = "(?P<word>a)(b)";
    let expected = Parser::new().parse(pattern).unwrap();
    assert_eq!(parser.parse(pattern).unwrap(), expected);
    assert_eq!(parser.parse(pattern).unwrap(), expected);

    parser.parse("(?x)a b").unwrap();
    assert_eq!(
        parser.parse("a b").unwrap(),
        Parser::new().parse("a b").unwrap()
    );
}

#[test]
fn parser_can_recover_after_an_error() {
    let mut parser = Parser::new();
    assert!(parser.parse("(?P<first>a)(").is_err());
    let pattern = "(?P<first>b)";
    assert_eq!(
        parser.parse(pattern).unwrap(),
        Parser::new().parse(pattern).unwrap()
    );
}
