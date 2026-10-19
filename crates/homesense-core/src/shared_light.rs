//! Physical planning for two lights sharing a supply and dimmer columns also
//! used by two separately supplied reading groups. No addresses or authority
//! belong here. Callers must obtain complete, fresh observations in one current
//! generation, authorize every expanded output, and admit the ordered plan as
//! one bounded operation. A plan is neither a bus transaction nor feedback.
use crate::{Command, LevelTransition, Percent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    CommonSupply,
    LeftSupply,
    RightSupply,
    PrimaryLevel,
    SecondaryLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Primary,
    Secondary,
}

/// Physical register values, before logical supply gating. Reading has its own
/// dimmer column; its two fixtures use the left/right supplies. Those supplies
/// also feed fixtures on both primary and secondary dimmer columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    pub common_supply: bool,
    pub left_supply: bool,
    pub right_supply: bool,
    pub primary_level: Percent,
    pub secondary_level: Percent,
    pub reading_level: Percent,
}

impl Observed {
    /// Raw nonzero registers behind an open supply are not lit fixtures.
    pub fn light_level(self, light: Light) -> Percent {
        if !self.common_supply {
            return zero();
        }
        match light {
            Light::Primary => self.primary_level,
            Light::Secondary => self.secondary_level,
        }
    }

    pub fn reading_active(self) -> bool {
        self.reading_level.get() != 0 && (self.left_supply || self.right_supply)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub writes: Vec<(Output, Command)>,
    /// Expected physical registers if all writes succeed. Never publish this
    /// as confirmed state before receiving the corresponding physical feedback.
    pub expected: Observed,
}

/// The primary control exits both reading groups. The secondary control keeps
/// actual reading active (its middle fixtures may follow the shared column),
/// but exits night-only operation. Both controls preserve the other common
/// light's *actual* state, not a latent dimmer value behind a disabled supply.
/// Off zeros only the target column; the peer's supply remains on when needed.
pub fn plan(observed: Observed, light: Light, level: Percent) -> Plan {
    let preserve_reading = light == Light::Secondary && observed.reading_active();
    let mut expected = observed;
    expected.primary_level = observed.light_level(Light::Primary);
    expected.secondary_level = observed.light_level(Light::Secondary);
    match light {
        Light::Primary => expected.primary_level = level,
        Light::Secondary => expected.secondary_level = level,
    }
    expected.common_supply =
        expected.primary_level.get() != 0 || expected.secondary_level.get() != 0;
    if !preserve_reading {
        expected.left_supply = false;
        expected.right_supply = false;
    }

    let mut writes = Vec::with_capacity(5);
    // De-energize excluded groups before touching shared dimmer columns. Do not
    // pulse either retained reading supply, even during an Off operation.
    if !preserve_reading {
        writes.push((Output::LeftSupply, Command::Switch(false)));
        writes.push((Output::RightSupply, Command::Switch(false)));
    }
    if !expected.common_supply {
        writes.push((Output::CommonSupply, Command::Switch(false)));
    }
    // Normalize a latent peer before energizing the common supply. This avoids
    // unintentionally lighting the primary when opening only the secondary.
    let peer = match light {
        Light::Primary => (Output::SecondaryLevel, expected.secondary_level),
        Light::Secondary => (Output::PrimaryLevel, expected.primary_level),
    };
    let target = match light {
        Light::Primary => Output::PrimaryLevel,
        Light::Secondary => Output::SecondaryLevel,
    };
    writes.push((peer.0, immediate(peer.1)));
    writes.push((target, immediate(level)));
    if expected.common_supply {
        writes.push((Output::CommonSupply, Command::Switch(true)));
    }
    Plan { writes, expected }
}

fn zero() -> Percent {
    Percent::new(0).expect("zero percent")
}

fn immediate(level: Percent) -> Command {
    Command::SetLevel {
        level,
        transition: LevelTransition::Immediate,
    }
}
