use actix_web::{get, web::Data, HttpResponse};
use std::sync::Mutex;

use crate::glow::State;

const INDEX_HTML: &str = include_str!("../web/index.html");

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
