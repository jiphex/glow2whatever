use core::panic;
use std::{sync::Mutex, time::Duration};

use actix_web::{
    get,
    web::{self, Data},
    App, HttpResponse, HttpServer,
};
use glow2whatever::glow::{GlowPacket, Packet};
use lazy_static::lazy_static;
use prometheus::{IntGaugeVec, Opts, Registry, TextEncoder};
use rumqttc::{AsyncClient, Event::Incoming, MqttOptions, Packet::Publish, QoS};

lazy_static! {
    pub static ref REGISTRY: Registry =
        Registry::new_custom(Some("glow".to_string()), None).expect("registry can be created");
    pub static ref CURRENT_POWER: IntGaugeVec = IntGaugeVec::new(
        Opts::new("current_power_wh", "Current meter power (kWh)"),
        &["mpan"],
    )
    .expect("power metric can be created");
    pub static ref TOTAL_ENERGY: IntGaugeVec = IntGaugeVec::new(
        Opts::new("total_energy_watts", "Cumulative energy (W)"),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref CUMULATIVE_ENERGY_DAY: IntGaugeVec = IntGaugeVec::new(
        Opts::new("cumulative_energy_day", "Cumulative energy - day (W)"),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref CUMULATIVE_ENERGY_WEEK: IntGaugeVec = IntGaugeVec::new(
        Opts::new("cumulative_energy_week", "Cumulative energy - week (W)"),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref CUMULATIVE_ENERGY_MONTH: IntGaugeVec = IntGaugeVec::new(
        Opts::new("cumulative_energy_month", "Cumulative energy - month (W)"),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref IMPORT_UNIT_PRICE: IntGaugeVec = IntGaugeVec::new(
        Opts::new("import_unit_price", "Cost per unit/kWh (centipence)"),
        &["mpan", "supplier"],
    )
    .expect("power metric can be created");
    pub static ref IMPORT_PRICE_STANDING_CHARGE: IntGaugeVec = IntGaugeVec::new(
        Opts::new("import_standing_charge", "Standing charge (centipence)"),
        &["mpan", "supplier"],
    )
    .expect("power metric can be created");
}

async fn register_metrics() {
    REGISTRY.register(Box::new(CURRENT_POWER.clone())).unwrap();
    REGISTRY.register(Box::new(TOTAL_ENERGY.clone())).unwrap();
    REGISTRY
        .register(Box::new(CUMULATIVE_ENERGY_DAY.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(CUMULATIVE_ENERGY_WEEK.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(CUMULATIVE_ENERGY_MONTH.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(IMPORT_UNIT_PRICE.clone()))
        .unwrap();
    REGISTRY
        .register(Box::new(IMPORT_PRICE_STANDING_CHARGE.clone()))
        .unwrap();
}

async fn glow_subscribe(client: &mut AsyncClient) {
    client
        .subscribe("glow/+/SENSOR/+", QoS::AtMostOnce)
        .await
        .unwrap();
    client
        .subscribe("glow/+/STATE/+", QoS::AtMostOnce)
        .await
        .unwrap();
}

async fn handle_publish(packet: Packet) -> Result<(), &'static str> {
    // println!("packet");
    match packet.packet {
        GlowPacket::Sensor(s) => {
            if let Some(met) = s.electricitymeter {
                // println!("power {:?}", met.energy);
                let labels = &[met.energy.import.mpan.as_str()];
                CURRENT_POWER
                    .with_label_values(labels)
                    .set((met.power.value * 1000_f64) as i64);
                TOTAL_ENERGY
                    .with_label_values(labels)
                    .set((met.energy.import.cumulative * 1000_f64) as i64);
                CUMULATIVE_ENERGY_DAY
                    .with_label_values(labels)
                    .set((met.energy.import.day * 1000_f64) as i64);
                CUMULATIVE_ENERGY_WEEK
                    .with_label_values(labels)
                    .set((met.energy.import.week * 1000_f64) as i64);
                CUMULATIVE_ENERGY_MONTH
                    .with_label_values(labels)
                    .set((met.energy.import.month * 1000_f64) as i64);
                let price_labels = &[
                    met.energy.import.mpan.as_str(),
                    met.energy.import.supplier.as_str(),
                ];
                IMPORT_UNIT_PRICE
                    .with_label_values(price_labels)
                    .set((met.energy.import.price.unitrate * 1000_f64) as i64);
                IMPORT_PRICE_STANDING_CHARGE
                    .with_label_values(price_labels)
                    .set((met.energy.import.price.standingcharge * 1000_f64) as i64);
            }
            Ok(())
        }
        GlowPacket::State(_) => Ok(()),
    }
}

async fn run_mqtt() {
    let mut mqttoptions = MqttOptions::new("rumqtt-sync", "boris.xyzzy.uk", 1883);
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (mut client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
    glow_subscribe(&mut client).await;
    // println!("subscriptions done");
    loop {
        // println!("waiting for packet");
        match eventloop.poll().await {
            Ok(event) => match event {
                Incoming(Publish(evi)) => {
                    tokio::spawn(handle_publish(evi.try_into().unwrap()));
                }
                _ => continue,
            },
            Err(_) => {
                panic!("bad mqtt poll");
            }
        }
    }
}

#[get("/metrics")]
async fn metrics(cache: web::Data<Mutex<String>>) -> HttpResponse {
    let data = cache.as_ref().lock().unwrap();
    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4")
        .insert_header(("header", "value"))
        .body(data.clone())
}

async fn run_export_cacher(mc: Data<Mutex<String>>) {
    loop {
        // println!("gathering metrics");
        tokio::time::sleep(Duration::from_secs(10)).await;
        let encoder = TextEncoder::new();
        let r_metrics = REGISTRY.gather();
        let metout = encoder.encode_to_string(&r_metrics).unwrap();
        let mut d = mc.as_ref().lock().unwrap();
        *d = metout;
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tokio::spawn(run_mqtt());
    register_metrics().await;
    // let mc = Arc::new(Mutex::new(String::new()));
    let d = Data::new(Mutex::new(String::from("# metrics will be available soon")));
    tokio::spawn(run_export_cacher(d.clone()));
    HttpServer::new(move || App::new().app_data(Data::clone(&d)).service(metrics))
        .bind(("::1", 8080))?
        .run()
        .await?;
    todo!()
}
