pub mod capabilities;
pub mod parser;

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    hash::Hash,
    io::{self, BufRead, BufReader},
    path::Path,
};

use flexstr::{SharedStr, ToSharedStr};

use capabilities::CapSet;
use tracing::info;

#[derive(Debug, Clone)]
pub enum Action {
    Require(HashSet<CapSet>),
    Direct,
    Reject,
}

impl Default for Action {
    fn default() -> Self {
        Self::Require(Default::default())
    }
}

impl From<parser::RuleAction> for Action {
    fn from(value: parser::RuleAction) -> Self {
        match value {
            parser::RuleAction::Direct => Self::Direct,
            parser::RuleAction::Reject => Self::Reject,
            parser::RuleAction::Require(caps) => {
                let mut set = HashSet::new();
                set.insert(caps);
                Self::Require(set)
            }
        }
    }
}

impl Action {
    fn len(&self) -> usize {
        match self {
            Self::Direct | Self::Reject => 1,
            Self::Require(set) => set.len(),
        }
    }

    fn extend(&mut self, other: Self) {
        match other {
            Self::Direct | Self::Reject => *self = other,
            Self::Require(new_caps) => {
                if let Self::Require(caps) = self {
                    caps.extend(new_caps.into_iter())
                } else {
                    *self = Self::Require(new_caps)
                }
            }
        }
    }
}

#[derive(Default)]
struct RuleSet<K: Eq + Hash>(HashMap<K, Action>);

type ListenPortRuleSet = RuleSet<u16>;
type DstDomainRuleSet = RuleSet<SharedStr>;

impl<K: Eq + Hash> RuleSet<K> {
    fn add(&mut self, key: K, action: parser::RuleAction) {
        // TODO: warning duplicated rules
        let value = self.0.entry(key).or_default();
        value.extend(action.into())
    }

    fn get<'a>(&'a self, key: &'a K) -> impl Iterator<Item = &'a Action> {
        self.0.get(key).into_iter()
    }
}

impl DstDomainRuleSet {
    fn get_recursive<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Action> {
        let name = name.trim_end_matches('.'); // Add back later
        let mut skip = name.len() + 1; // pretend ending with dot
        let parts = name.rsplit('.').map(move |part| {
            skip -= part.len() + 1; // +1 for the dot
            &name[skip..]
        });
        ["."] // add back the dot
            .into_iter()
            .chain(parts)
            .filter_map(|key| self.0.get(key))
    }
}

#[derive(Default)]
pub struct Policy {
    default_action: Action,
    listen_port_ruleset: ListenPortRuleSet,
    dst_domain_ruleset: DstDomainRuleSet,
}

impl Policy {
    pub fn load<R: BufRead>(read: R) -> io::Result<Self> {
        let mut router: Self = Default::default();
        for line in read.lines() {
            match parser::line_no_ending(&line?) {
                Ok((_, None)) => (),
                Ok((_, Some(rule))) => router.add_rule(rule),
                Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err.to_owned())),
            }
        }
        Ok(router)
    }

    pub fn load_from_file<T: AsRef<Path>>(path: T) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let this = Self::load(reader)?;
        info!("policy: {} rule(s) loaded", this.rule_count());
        Ok(this)
    }

    fn add_rule(&mut self, rule: parser::Rule) {
        let parser::Rule { filter, action } = rule;
        match filter {
            parser::RuleFilter::Default => self.default_action.extend(action.into()),
            parser::RuleFilter::ListenPort(port) => {
                self.listen_port_ruleset.add(port, action);
            }
            parser::RuleFilter::Sni(parts) => {
                self.dst_domain_ruleset.add(parts.to_shared_str(), action);
            }
        }
    }

    pub fn rule_count(&self) -> usize {
        self.listen_port_ruleset
            .0
            .values()
            .chain(self.dst_domain_ruleset.0.values())
            .fold(0, |acc, v| acc + v.len())
    }

    pub fn matches(&self, listen_port: Option<u16>, dst_domain: Option<SharedStr>) -> Action {
        let mut action: Action = self.default_action.clone();
        if let Some(port) = listen_port {
            self.listen_port_ruleset
                .get(&port)
                .for_each(|a| action.extend(a.clone()))
        }
        if let Some(name) = dst_domain {
            self.dst_domain_ruleset
                .get_recursive(&name)
                .for_each(|a| action.extend(a.clone()));
        }
        action
    }
}

#[test]
fn test_policy_listen_port() {
    use capabilities::CheckAllCapsMeet;

    let rules = "
        listen port 1 require a
        listen port 2 require b
        listen port 2 require c or d
    ";
    let policy = Policy::load(rules.as_bytes()).unwrap();
    assert_eq!(3, policy.rule_count());
    let p1 = match policy.matches(Some(1), None) {
        Action::Require(a) => a,
        _ => panic!(),
    };
    let p2 = match policy.matches(Some(2), None) {
        Action::Require(a) => a,
        _ => panic!(),
    };
    let abc = CapSet::new(["a", "b", "c"].into_iter());
    let bc = CapSet::new(["b", "c"].into_iter());
    let c = CapSet::new(["c"].into_iter());
    assert!(p1.all_meet_by(&abc));
    assert!(!p1.all_meet_by(&bc));
    assert!(!p1.all_meet_by(&c));
    assert!(p2.all_meet_by(&abc));
    assert!(p2.all_meet_by(&bc));
    assert!(!p2.all_meet_by(&c));
}

#[test]
fn test_policy_get_domain_caps_requirements() {
    let policy = Policy::load(
        "
        dst domain . require root
        dst domain com. require com
        dst domain example.com require example
    "
        .as_bytes(),
    )
    .unwrap();
    let set = policy.dst_domain_ruleset;
    assert_eq!(3, set.get_recursive("test.example.com").count());
    assert_eq!(3, set.get_recursive("example.com").count());
    assert_eq!(2, set.get_recursive("com").count());
    assert_eq!(1, set.get_recursive("net").count());
}

#[test]
fn test_policy_action() {
    let rules = "
        default require def
        listen port 1 require a
        listen port 2 direct
        dst domain test require c
        dst domain d.test direct
    ";
    let policy = Policy::load(rules.as_bytes()).unwrap();
    // listen-port/direct override default/require
    let direct1 = policy.matches(Some(2), Some("abcd".into()));
    assert!(matches!(direct1, Action::Direct));
    // d.test/direct override others
    let direct2 = policy.matches(Some(1), Some("a.d.test".into()));
    assert!(matches!(direct2, Action::Direct));
    // just default/require
    let require1 = policy.matches(Some(3), Some("abcd".into()));
    assert!(matches!(require1, Action::Require(a) if a.len() == 1));
    // default/require + dst-domain/require
    let require2 = policy.matches(None, Some("test".into()));
    assert!(matches!(require2, Action::Require(a) if a.len() == 2));
    // default/require + dst-domain/require + listen-port/require
    let require3 = policy.matches(Some(1), Some("test".into()));
    assert!(matches!(require3, Action::Require(a) if a.len() == 3));
}
