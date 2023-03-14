use flexstr::{shared_str, SharedStr, ToCase};
use nom::{
    branch::alt,
    bytes::complete::{tag_no_case, take_till1},
    character::complete::{char, line_ending, not_line_ending, space0, space1, u16},
    combinator::{opt, recognize, verify},
    multi::{fold_many0, many1, separated_list1},
    sequence::tuple,
    IResult, Parser,
};

use super::capabilities::CapSet;

#[derive(Debug, PartialEq, Eq)]
pub enum RuleFilter {
    ListenPort(u16),
    Sni(SharedStr),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RuleAction {
    Require(CapSet),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Rule {
    pub filter: RuleFilter,
    pub action: RuleAction,
}

fn port_number(input: &str) -> IResult<&str, u16> {
    verify(u16, |&n| n != 0)(input)
}

fn id_chars(input: &str) -> IResult<&str, &str> {
    take_till1(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')(input)
}

fn domain_name_part(input: &str) -> IResult<&str, SharedStr> {
    tuple((id_chars, opt(char('.'))))
        .map(|(name, _)| name.into())
        .parse(input)
}

fn domain_name_root(input: &str) -> IResult<&str, ()> {
    char('.').map(|_| ()).parse(input)
}

fn domain_name(input: &str) -> IResult<&str, SharedStr> {
    alt((
        recognize(many1(domain_name_part)).map(|n| {
            match n.strip_suffix('.') {
                Some(n) => SharedStr::from(n),
                None => n.into(),
            }
            .to_lower()
        }),
        domain_name_root.map(|_| shared_str!(".")),
    ))(input)
}

fn filter_sni(input: &str) -> IResult<&str, RuleFilter> {
    tuple((tag_no_case("sni"), space1, domain_name))
        .map(|(_, _, parts)| RuleFilter::Sni(parts))
        .parse(input)
}

fn filter_listen_port(input: &str) -> IResult<&str, RuleFilter> {
    tuple((tag_no_case("listen-port"), space1, port_number))
        .map(|(_, _, n)| RuleFilter::ListenPort(n))
        .parse(input)
}

fn rule_filter(input: &str) -> IResult<&str, RuleFilter> {
    alt((filter_sni, filter_listen_port))(input)
}

fn caps1(input: &str) -> IResult<&str, Vec<SharedStr>> {
    separated_list1(
        tuple((space1, tag_no_case("or"), space1)),
        id_chars.map(SharedStr::from),
    )(input)
}

fn action_require(input: &str) -> IResult<&str, RuleAction> {
    tuple((tag_no_case("require"), space1, caps1))
        .map(|(_, _, caps)| RuleAction::Require(CapSet::new(caps.into_iter())))
        .parse(input)
}

fn rule_action(input: &str) -> IResult<&str, RuleAction> {
    action_require(input)
}

fn rule(input: &str) -> IResult<&str, Rule> {
    tuple((rule_filter, space1, rule_action))
        .map(|(filter, _, action)| Rule { filter, action })
        .parse(input)
}

fn comment(input: &str) -> IResult<&str, ()> {
    tuple((char('#'), not_line_ending)).map(|_| ()).parse(input)
}

pub fn line(input: &str) -> IResult<&str, Option<Rule>> {
    tuple((space0, opt(rule), space0, opt(comment), line_ending))
        .map(|(_, rule, _, _, _)| rule)
        .parse(input)
}

pub fn line_no_ending(input: &str) -> IResult<&str, Option<Rule>> {
    tuple((space0, opt(rule), space0, opt(comment)))
        .map(|(_, rule, _, _)| rule)
        .parse(input)
}

pub fn document(input: &str) -> IResult<&str, Vec<Rule>> {
    fold_many0(line, Vec::new, |mut acc, item| {
        if let Some(rule) = item {
            acc.push(rule);
        }
        acc
    })(input)
}

#[test]
fn test_parse_domain_name_root() {
    let (empty, parts) = domain_name(".").unwrap();
    assert!(empty.is_empty());
    assert_eq!(shared_str!("."), parts);
}

#[test]
fn test_parse_domain_name() {
    let (rem, parts) = domain_name("Test_-123.Example.Com.\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(shared_str!("test_-123.example.com"), parts);

    let (rem, parts) = domain_name("example\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(shared_str!("example"), parts);
}

#[test]
fn test_listen_port_filter() {
    let (rem, port) = filter_listen_port("listen-port 1234\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(RuleFilter::ListenPort(1234), port);
}

#[test]
fn test_sni_filter() {
    let (rem, parts) = filter_sni("sni test\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(RuleFilter::Sni(shared_str!("test")), parts);
}

#[test]
fn test_action_require() {
    let (rem, caps) = action_require("require a or b\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(
        RuleAction::Require(CapSet::new(["a", "b"].into_iter())),
        caps
    );
}

#[test]
fn test_rule() {
    let (_, rule) = rule("listen-port 1 require a\n").unwrap();
    assert_eq!(
        Rule {
            filter: RuleFilter::ListenPort(1),
            action: RuleAction::Require(CapSet::new(["a"].into_iter()))
        },
        rule
    );
}

#[test]
fn test_comment() {
    comment("# test\n").unwrap();
    comment("#\n").unwrap();
}

#[test]
fn test_empty_line() {
    let (rem, rule) = line("\n").unwrap();
    assert!(rem.is_empty());
    assert_eq!(None, rule);
    let (_, rule) = line("  \n").unwrap();
    assert_eq!(None, rule);
    let (_, rule) = line("# test\n").unwrap();
    assert_eq!(None, rule);
    let (_, rule) = line("  # test\n").unwrap();
    assert_eq!(None, rule);
    let (_, rule) = line("#\n").unwrap();
    assert_eq!(None, rule);
}

#[test]
fn test_document() {
    let (_, rules) = document(
        "
        sni test require a # test\n\n\n
        # comment\n\
        # comment
        listen-port 123 require a or b   \n\
        sni test require b
        # end
    ",
    )
    .unwrap();
    assert_eq!(3, rules.len());
}
