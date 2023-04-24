use std::time::Duration;

use buttplug::core::{
    connector::{ButtplugRemoteClientConnector, ButtplugWebsocketClientTransport},
    message::serializer::ButtplugClientJSONSerializer,
};
use glow2whatever::glow::{GlowPacket, Packet};
use rumqttc::{AsyncClient, Event::Incoming, MqttOptions, Packet::Publish, QoS};

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

#[tokio::main]
async fn main() {
    let bc: buttplug::client::ButtplugClient = buttplug::client::ButtplugClient::new("glow2butt");
    let connector = ButtplugRemoteClientConnector::<
        ButtplugWebsocketClientTransport,
        ButtplugClientJSONSerializer,
    >::new(ButtplugWebsocketClientTransport::new_insecure_connector(
        "ws://localhost:12345",
    ));
    bc.connect(connector)
        .await
        .expect("unable to connect to server");

    let mut mqttoptions = MqttOptions::new("rumqtt-sync", "boris.xyzzy.uk", 1883);
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    let (mut client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
    glow_subscribe(&mut client).await;

    loop {
        let notification = eventloop.poll().await.unwrap();
        let event = notification;
        match event {
            Incoming(evi) => match evi {
                Publish(pi) => {
                    let ps: Packet = pi.try_into().unwrap();
                    println!("{ps:?}");
                    if let GlowPacket::Sensor(s) = ps.packet {
                        let pwr = s.electricitymeter.unwrap().power.value;
                        let out_pwr = (1.0 * pwr.clamp(0.0, 1.0)) * 0.8;
                        println!("pwr: {pwr}, out_pwr: {out_pwr}");

                        for dev in bc.devices() {
                            dev.vibrate(&buttplug::client::ScalarValueCommand::ScalarValue(
                                out_pwr,
                            ))
                            .await
                            .unwrap();
                        }
                    }
                }
                _ => continue,
            },
            _ => continue,
        }
    }
}
