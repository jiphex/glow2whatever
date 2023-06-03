use chrono::{DateTime, Utc};
use rumqttc::Publish;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Packet {
    pub id: String,
    pub packet: GlowPacket,
}

#[derive(Debug, thiserror::Error)]
pub enum GlowError {
    #[error("unable to parse topic")]
    BadTopic,
    #[error("failed to parse packet payload")]
    BadPayload(#[from] serde_json::Error),
}

impl TryFrom<Publish> for Packet {
    type Error = GlowError;
    fn try_from(value: Publish) -> Result<Self, Self::Error> {
        let topic_parts: Vec<&str> = value.topic.split('/').collect();
        let id = topic_parts.get(1).ok_or(GlowError::BadTopic)?;
        let stype = topic_parts.get(2).ok_or(GlowError::BadTopic)?;
        match *stype {
            "SENSOR" => {
                let psp: Sensor = serde_json::from_slice(&value.payload)?;
                Ok(Packet {
                    id: id.to_string(),
                    packet: GlowPacket::Sensor(psp),
                })
            }
            "STATE" => {
                let psp: State = serde_json::from_slice(&value.payload)?;
                Ok(Packet {
                    id: id.to_string(),
                    packet: GlowPacket::State(psp),
                })
            }
            _ => Err(GlowError::BadTopic),
        }
    }
}

#[derive(Debug)]
pub enum GlowPacket {
    State(State),
    Sensor(Sensor),
}

#[derive(PartialEq, Debug, Deserialize, Serialize)]
pub struct StateHan {
    pub rssi: i32,
    pub status: String,
    pub lqi: u32,
}

#[derive(PartialEq, Debug, Deserialize, Serialize)]
pub struct State {
    pub timestamp: DateTime<Utc>,
    pub software: String,
    pub hardware: String,
    pub wifistamac: String,
    pub smetsversion: String,
    pub eui: String,
    pub zigbee: String,
    pub han: StateHan,
}

#[derive(PartialEq, Debug, Deserialize)]
pub struct SensorEnergyExport {
    pub cumulative: f64,
    pub units: String,
}

impl Default for SensorEnergyExport {
    fn default() -> Self {
        Self {
            cumulative: Default::default(),
            units: "kWh".to_string(),
        }
    }
}

#[derive(PartialEq, Debug, Deserialize)]
pub struct SensorEnergyPrice {
    pub unitrate: f64,
    pub standingcharge: f64,
}

#[derive(PartialEq, Debug, Deserialize)]
pub struct SensorEnergyPower {
    pub value: f64,
    pub units: String,
}

#[derive(PartialEq, Debug, Deserialize)]
pub struct SensorEnergyImport {
    pub cumulative: f64,
    pub day: f64,
    pub week: f64,
    pub month: f64,
    pub units: String,
    pub mpan: String,
    pub supplier: String,
    pub price: SensorEnergyPrice,
}

#[derive(PartialEq, Debug, Deserialize)]
pub struct SensorEnergy {
    pub export: SensorEnergyExport,
    pub import: SensorEnergyImport,
}

#[derive(PartialEq, Debug, Deserialize)]
pub struct SensorElectricityMeter {
    pub timestamp: DateTime<Utc>,
    pub energy: SensorEnergy,
    pub power: SensorEnergyPower,
}

#[derive(PartialEq, Debug, Deserialize)]
pub struct Sensor {
    pub electricitymeter: Option<SensorElectricityMeter>,
}
