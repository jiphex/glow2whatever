use std::{
    net::{SocketAddr, ToSocketAddrs},
    sync::Mutex,
    time::Duration,
};

use actix_web::{
    get,
    web::{redirect, Data},
    App, HttpResponse, HttpServer,
};
use anyhow::anyhow;
use clap::Parser;
use glow2whatever::glow::{GlowPacket, Packet, State};
use lazy_static::lazy_static;
use prometheus::{Counter, IntGaugeVec, Opts, Registry, TextEncoder};
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
    pub static ref COLLECT_COUNT: Counter = Counter::new(
        "metric_collection_count",
        "Number of times metrics collected since programme start"
    )
    .expect("collection count metric can be created");
}

const INDEX_HTML: &str = include_str!("../../web/index.html");

#[tracing::instrument]
async fn register_metrics() -> prometheus::Result<()> {
    REGISTRY.register(Box::new(CURRENT_POWER.clone()))?;
    REGISTRY.register(Box::new(TOTAL_ENERGY.clone()))?;
    REGISTRY.register(Box::new(CUMULATIVE_ENERGY_DAY.clone()))?;
    REGISTRY.register(Box::new(CUMULATIVE_ENERGY_WEEK.clone()))?;
    REGISTRY.register(Box::new(CUMULATIVE_ENERGY_MONTH.clone()))?;
    REGISTRY.register(Box::new(IMPORT_UNIT_PRICE.clone()))?;
    REGISTRY.register(Box::new(IMPORT_PRICE_STANDING_CHARGE.clone()))?;
    REGISTRY.register(Box::new(COLLECT_COUNT.clone()))?;
    Ok(())
}

#[tracing::instrument(skip(client))]
async fn glow_subscribe(client: &mut AsyncClient) {
    client
        .subscribe("glow/+/SENSOR/+", QoS::AtMostOnce)
        .await
        .unwrap();
    client
        .subscribe("glow/+/STATE", QoS::AtMostOnce)
        .await
        .unwrap();
}

#[tracing::instrument(skip(mc, sensor))]
async fn handle_publish(
    packet: Packet,
    mc: Data<Mutex<String>>,
    sensor: Data<Mutex<Option<State>>>,
) -> anyhow::Result<()> {
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
            COLLECT_COUNT.inc();
            tokio::spawn(export_metrics(mc.clone()));
            Ok(())
        }
        GlowPacket::State(st) => {
            let mut sensor = sensor.lock().unwrap();
            debug!("sensor info {st:?}");
            sensor.replace(st);
            Ok(())
        }
    }
}

#[tracing::instrument(skip(mc, sensor))]
async fn run_mqtt(
    client_id: &str,
    connect_to: SocketAddr,
    mc: Data<Mutex<String>>,
    sensor: Data<Mutex<Option<State>>>,
) -> Result<(), ConnectionError> {
    let mut mqttoptions =
        MqttOptions::new(client_id, connect_to.ip().to_string(), connect_to.port());
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (mut client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
    glow_subscribe(&mut client).await;
    loop {
        debug!("waiting for mqtt packet");
        match eventloop.poll().await {
            Ok(event) => match event {
                Incoming(Publish(evi)) => {
                    tokio::spawn(handle_publish(
                        evi.try_into().unwrap(),
                        mc.clone(),
                        sensor.clone(),
                    ));
                }
                other => {
                    debug!("unhandled MQTT event: {:?}", other);
                    continue;
                }
            },
            Err(e) => return Err(e),
        }
    }
}

#[get("/")]
async fn index() -> HttpResponse {
    HttpResponse::Ok().body(INDEX_HTML)
}

#[get("/sensor")]
#[tracing::instrument(skip(sensor))]
async fn sensorpage(sensor: Data<Mutex<Option<State>>>) -> HttpResponse {
    let sensorm = sensor.lock().expect("panicked");
    match sensorm.as_ref() {
        None => HttpResponse::NotFound().body("sensor data not collected yet"),
        Some(s) => HttpResponse::Ok().json(s),
    }
}

#[get("/metrics")]
#[tracing::instrument(skip(cache))]
async fn metrics(cache: Data<Mutex<String>>) -> HttpResponse {
    let data = cache.as_ref().lock().unwrap();
    HttpResponse::Ok()
        .content_type(prometheus::TEXT_FORMAT)
        .body(data.clone())
}

/// Gather the metrics
#[tracing::instrument(skip_all)]
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
    /// Whether to include the process ID in the MQTT client id. This is useful
    /// when debugging as it will allow multiple instances of the application to
    /// be run, but should be avoided for production, as this will result in
    /// lost messages.
    #[arg(short, long, default_value_t = false)]
    client_id_include_pid: bool,

    /// Whether to enable jaeger tracing
    #[arg(long, default_value_t = false)]
    tracing_jaeger: bool,

    /// Whether to enable journald tracing
    #[arg(long, default_value_t = true)]
    tracing_journald: bool,

    /// Whether to enable stdout tracing
    #[arg(long, default_value_t = true)]
    tracing_stdout: bool,

    /// Listen address for the HTTP server which provides the metrics/sensor interfaces
    #[arg(short, long, default_value = "[::1]:8080")]
    listen_addr: SocketAddr,

    /// MQTT host/port to connect to
    #[arg(short, long, default_value = "example.com:1883")]
    mqtt_host: String,
}

fn host_from_str(input: &str) -> Option<SocketAddr> {
    input.to_socket_addrs().ok()?.next()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = CmdArgs::parse();

    let t_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    let t_sub_fmt = tracing_subscriber::fmt::layer().with_target(false);
    let t_journald = args
        .tracing_jaeger
        .then_some(tracing_journald::layer().ok())
        .flatten();
    let tr_otel = opentelemetry_jaeger::new_agent_pipeline().install_simple()?;
    let t_otel = args
        .tracing_jaeger
        .then_some(tracing_opentelemetry::layer().with_tracer(tr_otel));
    tracing_subscriber::registry()
        .with(t_filter)
        .with(t_otel)
        .with(t_sub_fmt)
        .with(t_journald)
        .init();
    debug!("command args: {args:?}");

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
    let sensor = Data::new(Mutex::new(Option::<State>::default()));
    let s2 = sensor.clone();
    let client_id = match args.client_id_include_pid {
        false => "rumqtt-sync".to_string(),
        true => format!("rumqtt-sync-{}", std::process::id()),
    };
    // Spawn a task to run the MQTT subscription
    tokio::spawn(async move {
        // This will await until the MQTT task finishes, which will be an error or not
        let out = run_mqtt(&client_id, mqtt_host, d2, sensor).await;

        // Pop the error back into the channel
        failtx.send(out).await.unwrap();
    });

    // Notify systemd that we're OK, if we can - it doesn't really matter if we
    // can't because we're probably running on Mac or something, so discard the
    // result
    let _ = sd_notify::notify(true, &[NotifyState::Ready]);

    // Now start the HTTP server
    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::clone(&d))
            .app_data(Data::clone(&s2))
            .wrap(TracingLogger::default())
            .service(metrics)
            .service(sensorpage)
            .service(index)
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
