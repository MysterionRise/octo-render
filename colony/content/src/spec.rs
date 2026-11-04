//! The records the RON files contain, in human units. These mirror the files one to
//! one through `serde`; conversion to kernel units is in [`crate::convert`].

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Vacuum,
    Gas,
    Liquid,
    Solid,
}

/// One element: `id` is its name and its position in the list is its index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElementSpec {
    pub id: String,
    pub state: State,
    pub shc_kj_per_kg_k: f64,
    pub conductivity_w_per_m_k: f64,
    pub density_kg_per_m3: f64,
    #[serde(default)]
    pub max_mass_kg: Option<f64>,
    #[serde(default)]
    pub freeze_k: Option<f64>,
    #[serde(default)]
    pub boil_k: Option<f64>,
    #[serde(default)]
    pub solid: Option<String>,
    #[serde(default)]
    pub liquid: Option<String>,
    #[serde(default)]
    pub gas: Option<String>,
    /// `#rrggbb`.
    pub colour: String,
}

/// What a building does, with its rates in human units (per second, watts, kilograms).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum KindSpec {
    Ladder,
    Door,
    Storage {
        capacity_kg: f64,
    },
    Heater {
        watts: f64,
    },
    OxygenGenerator {
        feedstock: String,
        feedstock_kg_per_s: f64,
        output: String,
        heat_w: f64,
    },
    HandCrank {
        watts: f64,
    },
    Battery {
        capacity_kj: f64,
    },
    Wire,
    GasPump {
        kg_per_s: f64,
    },
    Pipe,
    Vent,
    TemperatureSensor {
        above_k: f64,
    },
    PressureSensor {
        above_kg: f64,
    },
    SignalWire,
    And,
    Or,
    Not,
}

/// One building: `id` is its name and its position in the list is its index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildingSpec {
    pub id: String,
    pub size: (u8, u8),
    pub kind: KindSpec,
    /// `(element, kilograms)` a colonist must bring.
    #[serde(default)]
    pub cost: Option<(String, f64)>,
    #[serde(default = "default_build_ticks")]
    pub build_ticks: u16,
    pub colour: String,
    /// Positive generates, negative consumes, absent needs no power.
    #[serde(default)]
    pub power_w: f64,
}

fn default_build_ticks() -> u16 {
    20
}
