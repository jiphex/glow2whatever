use std::{
    net::{SocketAddr, ToSocketAddrs},
    time::Duration, sync::Mutex,
};

use actix_web::{
    get,
    web::{self, redirect, Data},
    App, HttpResponse, HttpServer,
};
use anyhow::anyhow;
use clap::Parser;
use glow2whatever::glow::{GlowPacket, Packet};
use lazy_static::lazy_static;
use prometheus::{IntGaugeVec, Opts, Registry, TextEncoder};
use rumqttc::{AsyncClient, ConnectionError, Event::Incoming, MqttOptions, Packet::Publish, QoS};
use sd_notify::NotifyState;
use tracing::{debug, info};
use tracing_actix_web::TracingLogger;
use tracing_subscriber::{
    prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt, EnvFilter,
};

lazy_static! {
    pub static ref REGISTRY: Registry =
        Registry::new_custom(Some("glow".to_string()), None).expect("registry can be created");
    pub static ref CURRENT_POWER: IntGaugeVec = IntGaugeVec::new(
        Opts::new("current_power_watts", "Current meter power (W)"),
        &["mpan"],
    )
    .expect("power metric can be created");
    pub static ref TOTAL_ENERGY: IntGaugeVec = IntGaugeVec::new(
        Opts::new("total_energy_watt_hours", "Cumulative energy (Wh)"),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref CUMULATIVE_ENERGY_DAY: IntGaugeVec = IntGaugeVec::new(
        Opts::new(
            "cumulative_energy_day_watt_hours",
            "Cumulative energy - day (Wh)"
        ),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref CUMULATIVE_ENERGY_WEEK: IntGaugeVec = IntGaugeVec::new(
        Opts::new(
            "cumulative_energy_week_watt_hours",
            "Cumulative energy - week (Wh)"
        ),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref CUMULATIVE_ENERGY_MONTH: IntGaugeVec = IntGaugeVec::new(
        Opts::new(
            "cumulative_energy_month_watt_hours",
            "Cumulative energy - month (Wh)"
        ),
        &["mpan"],
    )
    .expect("energy metric can be created");
    pub static ref IMPORT_UNIT_PRICE: IntGaugeVec = IntGaugeVec::new(
        Opts::new(
            "import_unit_price_centipence",
            "Cost per unit/kWh (centipence)"
        ),
        &["mpan", "supplier"],
    )
    .expect("unit price metric can be created");
    pub static ref IMPORT_PRICE_STANDING_CHARGE: IntGaugeVec = IntGaugeVec::new(
        Opts::new(
            "import_standing_charge_centipence",
            "Standing charge (centipence)"
        ),
        &["mpan", "supplier"],
    )
    .expect("standing charge metric can be created");
}

#[tracing::instrument]
async fn register_metrics() -> prometheus::Result<()> {
    REGISTRY.register(Box::new(CURRENT_POWER.clone()))?;
    REGISTRY.register(Box::new(TOTAL_ENERGY.clone()))?;
    REGISTRY.register(Box::new(CUMULATIVE_ENERGY_DAY.clone()))?;
    REGISTRY.register(Box::new(CUMULATIVE_ENERGY_WEEK.clone()))?;
    REGISTRY.register(Box::new(CUMULATIVE_ENERGY_MONTH.clone()))?;
    REGISTRY.register(Box::new(IMPORT_UNIT_PRICE.clone()))?;
    REGISTRY.register(Box::new(IMPORT_PRICE_STANDING_CHARGE.clone()))?;
    Ok(())
}

#[tracing::instrument]
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

#[tracing::instrument]
async fn handle_publish(packet: Packet, mc: Data<Mutex<String>>) -> anyhow::Result<()>{
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
            tokio::spawn(export_metrics(mc.clone()));
            Ok(())
        }
        GlowPacket::State(_) => Ok(()),
    }
}

#[tracing::instrument(skip_all)]
async fn run_mqtt(connect_to: SocketAddr, mc: Data<Mutex<String>>) -> Result<(), ConnectionError> {
    let mut mqttoptions = MqttOptions::new(
        format!("rumqtt-sync-{}", std::process::id()),
        connect_to.ip().to_string(),
        connect_to.port(),
    );
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (mut client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
    glow_subscribe(&mut client).await;
    loop {
        debug!("waiting for mqtt packet");
        match eventloop.poll().await {
            Ok(event) => match event {
                Incoming(Publish(evi)) => {
                    tokio::spawn(handle_publish(evi.try_into().unwrap(), mc.clone()));
                }
                other => {
                    debug!("other event: {:?}", other);
                    continue;
                }
            },
            Err(e) => return Err(e),
        }
    }
}

#[get("/metrics")]
#[tracing::instrument(level = "info", skip(cache))]
async fn metrics(cache: web::Data<Mutex<String>>) -> HttpResponse {
    let data = cache.as_ref().lock().unwrap();
    HttpResponse::Ok()
        .content_type(prometheus::TEXT_FORMAT)
        .body(data.clone())
}

/// Gather the metrics
#[tracing::instrument(level = "warn", skip_all)]
async fn export_metrics(mc: Data<Mutex<String>>) {
    info!("caching metrics for HTTP");
    let encoder = TextEncoder::new();
    let r_metrics = REGISTRY.gather();
    let metout = encoder.encode_to_string(&r_metrics).unwrap();
    let mut d = mc.as_ref().lock().unwrap();
    *d = metout;
}

#[derive(Debug, clap::Parser)]
struct CmdArgs {
    #[arg(short, long, default_value = "[::1]:8080")]
    listen_addr: SocketAddr,
    #[arg(short, long, default_value = "example.com:1883")]
    mqtt_host: String,
}

fn host_from_str(input: &str) -> Option<SocketAddr> {
    input.to_socket_addrs().ok()?.next()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let t_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    let t_sub_fmt = tracing_subscriber::fmt::layer().with_target(false);
    let t_journald = tracing_journald::layer().ok();
    tracing_subscriber::registry()
        .with(t_filter)
        .with(t_sub_fmt)
        .with(t_journald)
        .init();

    let args = CmdArgs::parse();

    // Parse the address we got from the command line
    let mqtt_host = host_from_str(&args.mqtt_host).ok_or(anyhow!("bad MQTT host"))?;

    // Register all the prometheus metrics with the default registry
    register_metrics()
        .await
        .expect("unable to register metrics");

    // Setup a channel to capture errors out of the MQTT channel
    let (failtx, mut failrx) = tokio::sync::mpsc::channel::<Result<(), ConnectionError>>(1);

    // Create a cache to store the metrics in
    let d = Data::new(Mutex::new(String::from("# metrics will be available soon")));

let d2 = d.clone();
    // Spawn a task to run the MQTT subscription
    tokio::spawn(async move {
        // This will await until the MQTT task finishes, which will be an error or not
        let out = run_mqtt(mqtt_host, d2).await;

        // Pop the error back into the channel
        failtx.send(out).await.unwrap();
    });


    // Spawn a task to run
    // tokio::spawn(run_export_cacher(d.clone(), metricrx));

    // Notify systemd that we're OK, if we can - it doesn't really matter if we can't
    let _ = sd_notify::notify(true, &[NotifyState::Ready]);

    // Now start the HTTP server
    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::clone(&d.clone()))
            .wrap(TracingLogger::default())
            .service(metrics)
            .service(redirect("/", "/metrics"))
    })
    .bind(args.listen_addr)?;

    // Wait for either the server to exit, or an error to pop out of the MQTT thread
    tokio::select! {
        _ = server.run() => {
            Ok(())
        }
        Some(err) = failrx.recv() => {
            err
        }
    }
    .map_err(|e| e.into())
}
