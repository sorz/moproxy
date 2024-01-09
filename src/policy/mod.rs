pub mod capabilities;
pub mod parser;

use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    fs::File,
    hash::Hash,
    io::{self, BufRead, BufReader},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
};

use flexstr::{SharedStr, ToSharedStr};

use capabilities::CapSet;
use ip_network_table_deps_treebitmap::{address::Address, IpLookupTable};
use tracing::info;

use self::parser::{Filter, Rule};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Action {
    priority: u8,
    pub action: ActionType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionType {
    Require(HashSet<CapSet>),
    Direct,
    Reject,
}

impl Default for ActionType {
    fn default() -> Self {
        Self::Require(Default::default())
    }
}

impl ActionType {
    fn wrap(self, priority: u8) -> Action {
        Action {
            priority,
            action: self,
        }
    }
}

impl From<ActionType> for Action {
    fn from(action: ActionType) -> Self {
        Self {
            priority: 0,
            action,
        }
    }
}

impl Action {
    fn len(&self) -> usize {
        match &self.action {
            ActionType::Direct | ActionType::Reject => 1,
            ActionType::Require(set) => set.len(),
        }
    }

    fn extend(&mut self, other: Self) {
        if self.priority < other.priority {
            *self = other;
        } else if self.priority == other.priority {
            match other.action {
                ActionType::Direct | ActionType::Reject => *self = other,
                ActionType::Require(new_caps) => {
                    if let ActionType::Require(caps) = &mut self.action {
                        caps.extend(new_caps.into_iter())
                    } else {
                        self.action = ActionType::Require(new_caps)
                    }
                }
            }
        }
        // Do nothing if self.priority > other.priority
    }
}

#[derive(Default)]
struct RuleSet<K: Eq + Hash>(HashMap<K, Action>);

type ListenPortRuleSet = RuleSet<u16>;
type DstDomainRuleSet = RuleSet<SharedStr>;

impl<K: Eq + Hash> RuleSet<K> {
    fn add(&mut self, key: K, action: Action) {
        // TODO: warning duplicated rules
        let value = self.0.entry(key).or_default();
        value.extend(action)
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

struct IpRuleSet<A: Address>(IpLookupTable<A, Action>);

type Ipv4RuleSet = IpRuleSet<Ipv4Addr>;
type Ipv6RuleSet = IpRuleSet<Ipv6Addr>;

impl<A: Address> Default for IpRuleSet<A> {
    fn default() -> Self {
        Self(IpLookupTable::new())
    }
}

impl<A: Address> IpRuleSet<A> {
    fn add(&mut self, net: (A, u8), action: Action) {
        let (ip, len) = net;
        let len = len as u32;
        match self.0.exact_match_mut(ip, len) {
            Some(item) => item.extend(action),
            None => {
                self.0.insert(ip, len, action);
            }
        }
    }

    fn get<'a>(&'a self, ip: &'a A) -> impl Iterator<Item = &'a Action> {
        self.0.matches(*ip).map(|(_, _, action)| action)
    }

    fn actions(&self) -> impl Iterator<Item = &Action> {
        self.0.iter().map(|(_, _, action)| action)
    }
}

#[derive(Debug, Default, Clone)]
pub struct RequestFeatures<S: AsRef<str>> {
    pub listen_port: Option<u16>,
    pub dst_ip: Option<IpAddr>,
    pub dst_domain: Option<S>,
}

#[derive(Default)]
pub struct Policy {
    default_action: Action,
    listen_port_ruleset: ListenPortRuleSet,
    dst_ipv4_ruleset: Ipv4RuleSet,
    dst_ipv6_ruleset: Ipv6RuleSet,
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
        let Rule { filter, action } = rule;
        match filter {
            Filter::Default => self.default_action.extend(action),
            Filter::ListenPort(port) => {
                self.listen_port_ruleset.add(port, action);
            }
            Filter::DstSni(parts) => {
                self.dst_domain_ruleset.add(parts.to_shared_str(), action);
            }
            Filter::DstIp((IpAddr::V4(ip), len)) => {
                self.dst_ipv4_ruleset.add((ip, len), action);
            }
            Filter::DstIp((IpAddr::V6(ip), len)) => {
                self.dst_ipv6_ruleset.add((ip, len), action);
            }
        }
    }

    pub fn rule_count(&self) -> usize {
        self.listen_port_ruleset
            .0
            .values()
            .chain(self.dst_domain_ruleset.0.values())
            .chain(self.dst_ipv4_ruleset.actions())
            .chain(self.dst_ipv6_ruleset.actions())
            .fold(0, |acc, v| acc + v.len())
    }

    pub fn matches<S: AsRef<str>>(&self, features: &RequestFeatures<S>) -> Action {
        let mut action: Action = self.default_action.clone();
        if let Some(port) = features.listen_port {
            self.listen_port_ruleset
                .get(&port)
                .for_each(|a| action.extend(a.clone()))
        }

        // Canonicalize IP address
        // Waiting for stablizion of IpAddr::to_canonical()
        let dst_ip = match features.dst_ip {
            None => None,
            ip @ Some(IpAddr::V4(_)) => ip,
            ip @ Some(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
                None => ip,
                Some(v4) => Some(IpAddr::V4(v4)),
            },
        };
        if let Some(IpAddr::V4(ip)) = dst_ip {
            self.dst_ipv4_ruleset
                .get(&ip)
                .for_each(|a| action.extend(a.clone()))
        }
        if let Some(IpAddr::V6(ip)) = dst_ip {
            self.dst_ipv6_ruleset
                .get(&ip)
                .for_each(|a| action.extend(a.clone()))
        }

        if let Some(name) = &features.dst_domain {
            self.dst_domain_ruleset
                .get_recursive(name.as_ref())
                .for_each(|a| action.extend(a.clone()));
        }
        action
    }
}

impl Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.action {
            ActionType::Direct => write!(f, "DIRECT"),
            ActionType::Reject => write!(f, "REJECT"),
            ActionType::Require(_) => write!(f, "REQUIRE"),
        }?;
        for _ in 0..self.priority {
            write!(f, "!")?;
        }
        if let ActionType::Require(ref caps) = self.action {
            let mut caps = Vec::from_iter(caps);
            caps.sort_unstable();
            match caps.first() {
                Some(cap) => write!(f, " {}", cap)?,
                None => write!(f, " NOTHING")?,
            }
            for cap in caps.iter().skip(1) {
                write!(f, " AND {}", cap)?;
            }
        }
        Ok(())
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
    let mut features: RequestFeatures<&'static str> = Default::default();
    features.listen_port = Some(1);
    let p1 = match policy.matches(&features).action {
        ActionType::Require(a) => a,
        _ => panic!(),
    };
    features.listen_port = Some(2);
    let p2 = match policy.matches(&features).action {
        ActionType::Require(a) => a,
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
fn test_policy_dst_ip() {
    use std::str::FromStr;

    let rules = "
        dst ip 0.0.0.0/0 require a4
        dst ip ::1 require a6
        dst ip 127.0.0.0/8 require b4
        dst ip 127.0.0.1 direct
    ";
    let policy = Policy::load(rules.as_bytes()).unwrap();
    assert_eq!(4, policy.rule_count());

    let mut features: RequestFeatures<&str> = Default::default();

    // Match 0.0.0.0/0
    features.dst_ip = IpAddr::from_str("1.2.3.4").ok();
    let action = policy.matches(&features).action;
    assert!(matches!(action, ActionType::Require(a) if a.len() == 1));

    // Match 0.0.0.0/0 & 127.0.0.0/8
    features.dst_ip = IpAddr::from_str("127.1.1.1").ok();
    let action1 = policy.matches(&features).action;
    features.dst_ip = IpAddr::from_str("::ffff:127.1.1.1").ok();
    let action2 = policy.matches(&features).action;
    assert_eq!(action1, action2);
    assert!(matches!(action1, ActionType::Require(a) if a.len() == 2));

    // Match 0.0.0.0/0 & 127.0.0.0/8, then override by 127.0.0.1/32 DIRECT
    features.dst_ip = IpAddr::from_str("127.0.0.1").ok();
    let action = policy.matches(&features).action;
    assert!(matches!(action, ActionType::Direct));

    // Match ::1/128
    features.dst_ip = IpAddr::from_str("::1").ok();
    let action = policy.matches(&features).action;
    assert!(matches!(action, ActionType::Require(a) if a.len() == 1));
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
    let direct1 = policy.matches(&RequestFeatures {
        listen_port: Some(2),
        dst_domain: Some("abcd"),
        ..Default::default()
    });
    assert!(matches!(direct1.action, ActionType::Direct));
    // d.test/direct override others
    let direct2 = policy.matches(&RequestFeatures {
        listen_port: Some(1),
        dst_domain: Some("a.d.test"),
        ..Default::default()
    });
    assert!(matches!(direct2.action, ActionType::Direct));
    // just default/require
    let require1 = policy.matches(&RequestFeatures {
        listen_port: Some(3),
        dst_domain: Some("abcd"),
        ..Default::default()
    });
    assert!(matches!(require1.action, ActionType::Require(a) if a.len() == 1));
    // default/require + dst-domain/require
    let require2 = policy.matches(&RequestFeatures {
        dst_domain: Some("test"),
        ..Default::default()
    });
    assert!(matches!(require2.action, ActionType::Require(a) if a.len() == 2));
    // default/require + dst-domain/require + listen-port/require
    let require3 = policy.matches(&RequestFeatures {
        listen_port: Some(1),
        dst_domain: Some("test"),
        ..Default::default()
    });
    assert!(matches!(require3.action, ActionType::Require(a) if a.len() == 3));
}

#[test]
fn test_policy_action_priority() {
    let rules = "
        default require! def
        listen port 1 reject # always ignore
        dst domain a require! a
        dst domain a.a reject! # same-level override
        dst domain a.a.a require!! aaa # level override
        dst domain a.a.a.a require!! aaaa #same-level append
    ";
    let policy = Policy::load(rules.as_bytes()).unwrap();
    let mut features: RequestFeatures<&'static str> = Default::default();

    features.listen_port = Some(10);
    let def = policy.matches(&features);
    assert!(matches!(&def.action, ActionType::Require(a) if a.len() == 1));
    assert_eq!(1, def.priority);

    features.listen_port = Some(1);
    let action = policy.matches(&features);
    assert_eq!(def, action);

    features.dst_domain = Some("a.a");
    let action = policy.matches(&features);
    assert!(matches!(&action.action, ActionType::Reject));

    features.dst_domain = Some("a.a.a");
    let action = policy.matches(&features);
    assert!(matches!(&action.action, ActionType::Require(a) if a.len() == 1));
    assert_eq!(2, action.priority);

    features.dst_domain = Some("a.a.a.a");
    let action = policy.matches(&features);
    assert!(matches!(&action.action, ActionType::Require(a) if a.len() == 2));
    assert_eq!(2, action.priority);
}

#[test]
fn test_action_type_display() {
    assert_eq!(
        "DIRECT",
        Action {
            action: ActionType::Direct,
            priority: 0
        }
        .to_string()
    );
    assert_eq!(
        "REJECT!!",
        Action {
            action: ActionType::Reject,
            priority: 2
        }
        .to_string()
    );
    assert_eq!("REQUIRE NOTHING", Action::default().to_string());
    let caps = HashSet::from_iter(vec![
        CapSet::new(["a"].into_iter()),
        CapSet::new(["b", "c"].into_iter()),
    ]);
    let action = ActionType::Require(caps);
    assert_eq!(
        "REQUIRE! a AND (b OR c)",
        Action {
            action,
            priority: 1
        }
        .to_string()
    );
}
