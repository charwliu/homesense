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
/// but opening it exits night-only operation. Explicitly closing an already
/// unpowered secondary preserves the existing bedside/night matrix. Both
/// controls preserve the other common light's *actual* state, not a latent
/// dimmer value behind a disabled supply.
/// Off zeros only the target column; the peer's supply remains on when needed.
pub fn plan(observed: Observed, light: Light, level: Percent) -> Plan {
    let preserve_bedside = light == Light::Secondary
        && (observed.reading_active() || level.get() == 0 && !observed.common_supply);
    let mut expected = observed;
    let peer_light = match light {
        Light::Primary => Light::Secondary,
        Light::Secondary => Light::Primary,
    };
    match light {
        Light::Primary => expected.primary_level = level,
        Light::Secondary => expected.secondary_level = level,
    }
    expected.common_supply = level.get() != 0 || observed.light_level(peer_light).get() != 0;
    let energizing = expected.common_supply && !observed.common_supply;
    // A latent column needs normalization only before enabling its supply.
    // Otherwise preserve the physical register and its independent fixtures.
    if energizing {
        match peer_light {
            Light::Primary => expected.primary_level = zero(),
            Light::Secondary => expected.secondary_level = zero(),
        }
    }
    if !preserve_bedside {
        expected.left_supply = false;
        expected.right_supply = false;
    }

    let mut writes = Vec::with_capacity(5);
    // De-energize excluded groups before touching shared dimmer columns. Do not
    // pulse either retained reading supply, even during an Off operation.
    if !preserve_bedside {
        writes.push((Output::LeftSupply, Command::Switch(false)));
        writes.push((Output::RightSupply, Command::Switch(false)));
    }
    if observed.common_supply && !expected.common_supply {
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
    if energizing {
        writes.push((peer.0, immediate(peer.1)));
    }
    writes.push((target, immediate(level)));
    if energizing {
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
