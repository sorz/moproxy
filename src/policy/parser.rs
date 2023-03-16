use std::collections::HashSet;

use flexstr::{shared_str, SharedStr, ToCase};
use nom::{
    branch::alt,
    bytes::complete::{tag_no_case, take_till1},
    character::complete::{char, not_line_ending, space0, space1, u16},
    combinator::{eof, opt, recognize, verify},
    multi::{many0_count, many1, separated_list0, separated_list1},
    sequence::tuple,
    IResult, Parser,
};

use super::{capabilities::CapSet, Action, ActionType};

#[derive(Debug, PartialEq, Eq)]
pub enum Filter {
    Default,
    ListenPort(u16),
    Sni(SharedStr),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Rule {
    pub filter: Filter,
    pub action: Action,
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

fn filter_dst_domain(input: &str) -> IResult<&str, Filter> {
    tuple((tag_no_case("dst domain"), space1, domain_name))
        .map(|(_, _, parts)| Filter::Sni(parts))
        .parse(input)
}

fn filter_listen_port(input: &str) -> IResult<&str, Filter> {
    tuple((tag_no_case("listen port"), space1, port_number))
        .map(|(_, _, n)| Filter::ListenPort(n))
        .parse(input)
}

fn filter_default(input: &str) -> IResult<&str, Filter> {
    tag_no_case("default").map(|_| Filter::Default).parse(input)
}

fn rule_filter(input: &str) -> IResult<&str, Filter> {
    alt((filter_dst_domain, filter_listen_port, filter_default))(input)
}

fn cap_name(input: &str) -> IResult<&str, SharedStr> {
    id_chars.map(SharedStr::from).parse(input)
}

fn caps1(input: &str) -> IResult<&str, Vec<SharedStr>> {
    separated_list1(tuple((space1, tag_no_case("or"), space1)), cap_name)(input)
}

fn action_priority(input: &str) -> IResult<&str, u8> {
    verify(many0_count(tag_no_case("!")), |n| *n <= 5)
        .map(|n| n as u8)
        .parse(input)
}

fn action_require(input: &str) -> IResult<&str, Action> {
    tuple((tag_no_case("require"), action_priority, space1, caps1))
        .map(|(_, priority, _, caps)| {
            let mut set = HashSet::new();
            set.insert(CapSet::new(caps.into_iter()));
            ActionType::Require(set).wrap(priority)
        })
        .parse(input)
}

fn action_direct(input: &str) -> IResult<&str, Action> {
    tuple((tag_no_case("direct"), action_priority))
        .map(|(_, priority)| ActionType::Direct.wrap(priority))
        .parse(input)
}

fn action_reject(input: &str) -> IResult<&str, Action> {
    tuple((tag_no_case("reject"), action_priority))
        .map(|(_, priority)| ActionType::Reject.wrap(priority))
        .parse(input)
}

fn rule_action(input: &str) -> IResult<&str, Action> {
    alt((action_require, action_direct, action_reject)).parse(input)
}

fn rule(input: &str) -> IResult<&str, Rule> {
    tuple((rule_filter, space1, rule_action))
        .map(|(filter, _, action)| Rule { filter, action })
        .parse(input)
}

fn comment(input: &str) -> IResult<&str, ()> {
    tuple((char('#'), not_line_ending)).map(|_| ()).parse(input)
}

pub fn capabilities(input: &str) -> IResult<&str, CapSet> {
    separated_list0(
        alt((recognize(tuple((space0, tag_no_case(","), space0))), space1)),
        cap_name,
    )
    .map(|caps| CapSet::new(caps.into_iter()))
    .parse(input)
}

pub fn line_no_ending(input: &str) -> IResult<&str, Option<Rule>> {
    alt((
        tuple((space0, opt(comment), space0, eof)).map(|_| None),
        tuple((space0, rule, space0, opt(comment), eof)).map(|(_, rule, _, _, _)| Some(rule)),
    ))
    .parse(input)
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
    let (rem, port) = filter_listen_port("listen port 1234\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(Filter::ListenPort(1234), port);
}

#[test]
fn test_dst_domain_filter() {
    let (rem, parts) = filter_dst_domain("dst domain test\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(Filter::Sni(shared_str!("test")), parts);
}

#[test]
fn test_dst_default_filter() {
    let (rem, parts) = filter_default("default\n").unwrap();
    assert_eq!("\n", rem);
    assert_eq!(Filter::Default, parts);
}

#[test]
fn test_action() {
    let (rem, action) = rule_action("require a or b\n").unwrap();
    assert_eq!("\n", rem);
    assert!(matches!(
        action.action,
        ActionType::Require(caps) if caps.iter().next().unwrap() == &CapSet::new(["a", "b"].into_iter()),
    ));
    let (_, action) = rule_action("direct\n").unwrap();
    assert_eq!(ActionType::Direct, action.action);
    let (_, action) = rule_action("reject\n").unwrap();
    assert_eq!(ActionType::Reject, action.action);
}

#[test]
fn test_action_priority() {
    let (_, action) = rule_action("require a").unwrap();
    assert_eq!(0, action.priority);
    let (_, action) = rule_action("require! a").unwrap();
    assert_eq!(1, action.priority);
    let (_, action) = rule_action("direct!!").unwrap();
    assert_eq!(2, action.priority);
    let (_, action) = rule_action("reject!!!!!").unwrap();
    assert_eq!(5, action.priority);
    assert!(rule_action("reject!!!!!!").is_err());
    assert!(rule_action("require!!!1 a").is_err());
}

#[test]
fn test_rule() {
    let (_, result) = rule("listen port 1 require!! a\n").unwrap();
    let mut set = HashSet::new();
    set.insert(CapSet::new(["a"].into_iter()));
    assert_eq!(
        Rule {
            filter: Filter::ListenPort(1),
            action: ActionType::Require(set).wrap(2)
        },
        result
    );
    assert!(rule("default require!!!1 a\n").is_err());
    assert!(rule("default require ! a\n").is_err());
}

#[test]
fn test_comment() {
    comment("# test\n").unwrap();
    comment("#\n").unwrap();
}

#[test]
fn test_line_no_ending() {
    let (_, rules) = line_no_ending("").unwrap();
    assert!(rules.is_none());
    let (_, rules) = line_no_ending("  \t ").unwrap();
    assert!(rules.is_none());
    let (_, rules) = line_no_ending("#").unwrap();
    assert!(rules.is_none());
    let (_, rules) = line_no_ending(" # test ").unwrap();
    assert!(rules.is_none());
    let (_, rules) = line_no_ending("dst domain test require a #1").unwrap();
    assert!(rules.is_some());
}

#[test]
fn test_line_no_ending_error() {
    assert!(line_no_ending("dst").is_err());
    assert!(line_no_ending("dst domain test error").is_err());
    assert!(line_no_ending("dst domain require a b").is_err());
}

#[test]
fn test_capabilities() {
    let (_, caps) = capabilities("a b  c ").unwrap();
    assert_eq!(
        CapSet::new(["a", "b", "c"].into_iter().map(SharedStr::from)),
        caps
    );
    let (_, caps) = capabilities("a, b  ,c,d,").unwrap();
    assert_eq!(
        CapSet::new("abcd".chars().into_iter().map(SharedStr::from)),
        caps
    );
    let (_, caps) = capabilities("  ").unwrap();
    assert!(caps.is_empty());
}
