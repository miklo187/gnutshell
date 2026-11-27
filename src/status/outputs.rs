//! Which audio devices there are to choose from, and how to switch to one.
//! Outputs and inputs work alike; what follows says "output" for either, with
//! sources in the place of sinks.
//!
//! An output is a port of a sound card, as in GNOME Settings: speakers,
//! headphones, a monitor over HDMI. Many cards offer only some of their ports
//! at a time, so using another one can mean changing the card's profile.
//! Sinks without a card (virtual or network outputs) count as outputs too.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Output,
    Input,
}

impl Direction {
    /// How a card profile names its part for the other direction.
    fn other_profile_part(self) -> &'static str {
        match self {
            Direction::Output => "input:",
            Direction::Input => "output:",
        }
    }
}

/// A card number meaning "none" (`PA_INVALID_INDEX`).
#[cfg(test)]
pub const NO_CARD: u32 = u32::MAX;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sink {
    pub name: String,
    pub description: String,
    pub card: u32,
    pub ports: Vec<String>,
    pub active_port: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub name: String,
    pub priority: u32,
    pub available: bool,
}

/// An output port of a card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Port {
    pub name: String,
    pub description: String,
    /// What is plugged in, e.g. a monitor's name.
    pub product: String,
    /// `false` only if the card knows nothing is plugged in.
    pub plugged: bool,
    /// The profiles that include this port.
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub index: u32,
    pub description: String,
    pub active_profile: String,
    pub ports: Vec<Port>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Port { card: u32, port: String },
    Sink(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub label: String,
    pub target: Target,
    pub active: bool,
}

/// The outputs to offer; `default` is the default sink's name.
pub fn list(cards: &[Card], sinks: &[Sink], default: &str) -> Vec<Output> {
    let current = sinks.iter().find(|s| s.name == default);
    let mut out = Vec::new();
    for card in cards {
        for port in card.ports.iter().filter(|p| p.plugged) {
            let name = if port.product.is_empty() { &card.description } else { &port.product };
            out.push(Output {
                label: format!("{} – {name}", port.description),
                target: Target::Port { card: card.index, port: port.name.clone() },
                active: current.is_some_and(|s| s.card == card.index && s.active_port == port.name),
            });
        }
    }
    for sink in sinks.iter().filter(|s| !cards.iter().any(|c| c.index == s.card)) {
        out.push(Output {
            label: sink.description.clone(),
            target: Target::Sink(sink.name.clone()),
            active: sink.name == default,
        });
    }
    out
}

/// The sink that has `port` of `card` right now, if its profile includes it.
pub fn sink_with_port<'a>(sinks: &'a [Sink], card: u32, port: &str) -> Option<&'a Sink> {
    sinks.iter().find(|s| s.card == card && s.ports.iter().any(|p| p == port))
}

/// The card profile to switch to for `port`: one that keeps the current
/// device of the other direction if there is such, then the card's favorite.
pub fn profile_for<'a>(card: &Card, port: &'a Port, direction: Direction) -> Option<&'a str> {
    let keep = direction.other_profile_part();
    let other = |profile: &str| profile.split('+').find(|part| part.starts_with(keep)).map(str::to_owned);
    let current = other(&card.active_profile);
    port.profiles
        .iter()
        .filter(|p| p.available)
        .max_by_key(|p| (other(&p.name) == current, p.priority))
        .map(|p| p.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, priority: u32) -> Profile {
        Profile { name: name.into(), priority, available: true }
    }

    fn port(name: &str, plugged: bool, profiles: Vec<Profile>) -> Port {
        Port { name: name.into(), description: name.to_uppercase(), plugged, profiles, ..Default::default() }
    }

    fn card() -> Card {
        Card {
            index: 49,
            description: "Built-in".into(),
            active_profile: "output:hdmi+input:analog".into(),
            ports: vec![
                Port { product: "Monitor".into(), ..port("hdmi", true, vec![profile("output:hdmi+input:analog", 5)]) },
                port(
                    "speaker",
                    true,
                    vec![profile("output:analog", 90), profile("output:analog+input:analog", 80)],
                ),
                port("headphones", false, vec![]),
            ],
        }
    }

    fn sink(name: &str, card: u32, port: &str) -> Sink {
        Sink {
            name: name.into(),
            description: format!("{name} desc"),
            card,
            ports: vec![port.into()],
            active_port: port.into(),
        }
    }

    #[test]
    fn lists_plugged_ports_and_cardless_sinks() {
        let sinks = [sink("hdmi-sink", 49, "hdmi"), sink("virtual", NO_CARD, "")];
        let out = list(&[card()], &sinks, "hdmi-sink");
        let summary: Vec<(&str, bool)> = out.iter().map(|o| (o.label.as_str(), o.active)).collect();
        assert_eq!(summary, [("HDMI – Monitor", true), ("SPEAKER – Built-in", false), ("virtual desc", false)]);
        assert!(list(&[card()], &sinks, "virtual")[2].active);
    }

    #[test]
    fn switching_keeps_the_input() {
        let card = card();
        let out = Direction::Output;
        assert_eq!(profile_for(&card, &card.ports[1], out), Some("output:analog+input:analog"));
        assert_eq!(profile_for(&card, &card.ports[2], out), None);
    }

    #[test]
    fn switching_the_input_keeps_the_output() {
        let card = card();
        let mic = port(
            "mic",
            true,
            vec![profile("input:analog", 90), profile("output:hdmi+input:analog", 5)],
        );
        assert_eq!(profile_for(&card, &mic, Direction::Input), Some("output:hdmi+input:analog"));
    }

    #[test]
    fn finds_the_sink_for_a_port() {
        let sinks = [sink("hdmi-sink", 49, "hdmi")];
        assert!(sink_with_port(&sinks, 49, "hdmi").is_some());
        assert!(sink_with_port(&sinks, 49, "speaker").is_none());
    }
}
