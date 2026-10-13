use wintermiles_core::devices::ant::{encode_message, AntParser};
use wintermiles_core::devices::ble::*;
use wintermiles_core::devices::profiles::*;
use wintermiles_core::devices::*;

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn decoder(id: ProfileId) -> PageDecoder {
    PageDecoder::new(id)
}

mod bluetooth {
    use super::*;

    #[test]
    fn heart_rate_8_and_16_bit() {
        assert_eq!(parse_heart_rate(&[0x00, 142]), Some(142.0));
        assert_eq!(parse_heart_rate(&[0x01, 0x2c, 0x01]), Some(300.0));
        assert_eq!(parse_heart_rate(&[]), None);
    }

    #[test]
    fn cycling_power_with_wheel_and_crank_data() {
        // flags 0x0030: wheel + crank revolution data present
        let m = parse_cycling_power(&[0x30, 0x00, 0xfa, 0x00, 0x10, 0x27, 0x00, 0x00, 0x00, 0x08, 0x64, 0x00, 0x00, 0x04]).unwrap();
        assert_eq!(m, CyclingPower { power: 250.0, wheel: Some((10000, 2048)), crank: Some((100, 1024)) });
        // flags 0x0001: pedal balance present, shifts nothing we read
        assert_eq!(parse_cycling_power(&[0x01, 0x00, 0x2c, 0x01, 0x64]).unwrap().power, 300.0);
        // negative power is legal (sint16)
        assert_eq!(parse_cycling_power(&[0x00, 0x00, 0xff, 0xff]).unwrap().power, -1.0);
        // truncated packets are rejected, not read out of bounds
        assert_eq!(parse_cycling_power(&[0x30, 0x00, 0xfa]), None);
    }

    #[test]
    fn speed_and_cadence() {
        assert_eq!(parse_csc(&[0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04, 0x02, 0x00, 0x00, 0x08]).unwrap(), Csc { wheel: Some((1, 1024)), crank: Some((2, 2048)) });
        assert_eq!(parse_csc(&[0x02, 0x05, 0x00, 0x00, 0x04]).unwrap(), Csc { wheel: None, crank: Some((5, 1024)) });
    }

    #[test]
    fn ftms_indoor_bike_data() {
        // flags 0x0244: cadence + power + heart rate; speed present because bit 0 is clear
        let m = parse_indoor_bike_data(&[0x44, 0x02, 0x10, 0x0e, 0xb4, 0x00, 0xc8, 0x00, 0x96]).unwrap();
        assert!(close(m.speed.unwrap(), 10.0, 1e-9));
        assert_eq!((m.cadence, m.power, m.hr), (Some(90.0), Some(200.0), Some(150.0)));
        // flags 0x0075: more-data set (no speed), cadence, distance, resistance, power
        let n = parse_indoor_bike_data(&[0x75, 0x00, 0xa0, 0x00, 1, 2, 3, 9, 0, 0x2c, 0x01]).unwrap();
        assert_eq!(n, IndoorBikeData { speed: None, cadence: Some(80.0), power: Some(300.0), hr: None });
    }

    #[test]
    fn ftms_control_point_commands() {
        assert_eq!(ftms_target_power(250.0), [0x05, 0xfa, 0x00]);
        // +5.5% grade, crr 0.004, cw 0.51 kg/m
        assert_eq!(ftms_simulation(0.055, 0.004, 0.51), [0x11, 0, 0, 0x26, 0x02, 40, 51]);
        // −3% grade is two's complement
        let down = ftms_simulation(-0.03, 0.004, 0.5);
        assert_eq!(i16::from_le_bytes([down[3], down[4]]), -300);
    }
}

mod ant_framing {
    use super::*;

    #[test]
    fn encodes_with_an_xor_checksum() {
        // Canonical reset message.
        assert_eq!(encode_message(0x4a, &[0]), [0xa4, 0x01, 0x4a, 0x00, 0xef]);
    }

    #[test]
    fn reassembles_split_messages_and_resyncs_past_garbage() {
        let a = encode_message(0x4e, &[0, 1, 2, 3, 4, 5, 6, 7, 8]);
        let b = encode_message(0x40, &[1, 0x4b, 0]);
        let mut stream = vec![0x00, 0xa4, 0x7f];
        stream.extend_from_slice(&a);
        stream.extend_from_slice(&b);
        let mut parser = AntParser::default();
        assert!(parser.push(&stream[..9]).is_empty());
        let rest = parser.push(&stream[9..]);
        assert_eq!(rest.iter().map(|m| m.id).collect::<Vec<_>>(), [0x4e, 0x40]);
        assert_eq!(rest[0].data, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(rest[1].data, [1, 0x4b, 0]);
    }
}

mod ant_profiles {
    use super::*;

    #[test]
    fn heart_rate() {
        let r = decoder(ProfileId::HeartRate).decode(&[0x04, 0, 0, 0, 0x10, 0x20, 0x05, 147], 0).unwrap();
        assert_eq!(r, Readings { hr: Some(147.0), ..Default::default() });
    }

    #[test]
    fn power_only_page_and_zero_when_the_event_count_freezes() {
        let mut d = decoder(ProfileId::Power);
        let page = |event: u8| [0x10, event, 0xff, 91, 0, 0, 0x2c, 0x01];
        let riding = Readings { power: Some(300.0), cadence: Some(91.0), ..Default::default() };
        assert_eq!(d.decode(&page(1), 0), Some(riding));
        assert_eq!(d.decode(&page(2), 1000), Some(riding));
        assert_eq!(d.decode(&page(2), 2000), Some(riding));
        assert_eq!(d.decode(&page(2), 4500), Some(Readings { power: Some(0.0), cadence: Some(0.0), ..Default::default() }));
    }

    #[test]
    fn crank_torque_only_power_meters() {
        let mut d = decoder(ProfileId::Power);
        // 1 rev/s (period 2048), 20 Nm average (Δtorque 640 per rev) → 2π·20 ≈ 125.7 W at 60 rpm
        assert_eq!(d.decode(&[0x12, 1, 1, 0xff, 0x00, 0x08, 0x80, 0x02], 10_000), None);
        let r = d.decode(&[0x12, 2, 2, 0xff, 0x00, 0x10, 0x00, 0x05], 11_000).unwrap();
        assert!(close(r.power.unwrap(), 125.66, 0.01));
        assert!(close(r.cadence.unwrap(), 60.0, 1e-9));
    }

    #[test]
    fn combined_speed_and_cadence() {
        let mut d = decoder(ProfileId::SpeedCadence);
        d.decode(&[0; 8], 0);
        // +1 crank rev and +4 wheel revs in 1024 ticks (1 s)
        let r = d.decode(&[0x00, 0x04, 0x01, 0x00, 0x00, 0x04, 0x04, 0x00], 1000).unwrap();
        assert!(close(r.cadence.unwrap(), 60.0, 1e-9));
        assert!(close(r.wheel_speed.unwrap(), 4.0 * 2.105, 1e-9));
    }

    #[test]
    fn fec_trainer_data_pages() {
        assert_eq!(decode_fec(&[0x19, 1, 85, 0, 0, 0xfa, 0x30, 0]), Some(Readings { cadence: Some(85.0), power: Some(250.0), ..Default::default() }));
        assert_eq!(decode_fec(&[0x19, 1, 0xff, 0, 0, 0x2c, 0x01, 0]), Some(Readings { power: Some(300.0), ..Default::default() }));
        let general = decode_fec(&[0x10, 25, 0, 0, 0x10, 0x27, 0xff, 0]).unwrap();
        assert!(close(general.wheel_speed.unwrap(), 10.0, 1e-9));
        assert_eq!(general.hr, None);
        assert_eq!(decode_fec(&[0x50, 0, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn fec_control_pages() {
        // 0% grade = 20000 (0x4E20); crr 0.004 = 80 × 5e-5
        assert_eq!(fec_track_resistance(0.0, 0.004), [0x33, 0xff, 0xff, 0xff, 0xff, 0x20, 0x4e, 80]);
        // +7.25% → 20725 = 0x50F5
        assert_eq!(fec_track_resistance(0.0725, 0.004)[5..7], [0xf5, 0x50]);
        assert_eq!(fec_track_resistance(-0.1, 0.004)[5..7], [0x38, 0x4a]);
        // 200 W = 800 quarter-watts
        assert_eq!(fec_target_power(200.0), [0x31, 0xff, 0xff, 0xff, 0xff, 0xff, 0x20, 0x03]);
        assert_eq!(fec_wind_resistance(0.51), [0x32, 0xff, 0xff, 0xff, 0xff, 51, 127, 100]);
        // 75 kg rider = 7500 (0x1D4C); 10 kg bike = 200 (0x0C8) → nibble 8, byte 0x0C
        assert_eq!(fec_user_config(75.0, 10.0), [0x37, 0x4c, 0x1d, 0xff, 0x8f, 0x0c, 70, 0x00]);
        assert_eq!(fec_pages(TrainerCommand::Simulation { grade: 0.0, crr: 0.004, cw: 0.51 }).len(), 2);
    }
}

#[test]
fn rev_counter_handles_wrap_and_stops() {
    let mut c = RevCounter::new(65536, 1024.0);
    c.update(65535, 65000, 0);
    // +2 revs across the 16-bit wrap, +1024 ticks across the time wrap
    let t = (65000 + 1024) % 65536;
    assert!(close(c.update(1, t, 1000), 120.0, 1e-9));
    assert!(close(c.update(1, t, 2000), 120.0, 1e-9));
    assert_eq!(c.update(1, t, 4000), 0.0);
}

mod hub {
    use super::*;

    fn dev(id: &str, kind: DeviceKind) -> DeviceInfo {
        DeviceInfo { id: id.into(), name: id.into(), transport: Transport::Ble, kind, status: DeviceStatus::Connected, controllable: kind == DeviceKind::Trainer, readings: Readings::default() }
    }

    #[test]
    fn prefers_the_most_direct_source_and_drops_stale_readings() {
        let mut hub = Hub::default();
        assert!(hub.add(dev("trainer", DeviceKind::Trainer)));
        assert!(hub.add(dev("pm", DeviceKind::Power)));
        assert!(hub.add(dev("strap", DeviceKind::Hr)));
        assert!(!hub.add(dev("pm", DeviceKind::Power)));
        hub.report("trainer", Readings { power: Some(200.0), cadence: Some(80.0), hr: Some(120.0), ..Default::default() }, 1000);
        hub.report("pm", Readings { power: Some(215.0), ..Default::default() }, 1000);
        hub.report("strap", Readings { hr: Some(150.0), ..Default::default() }, 1000);
        hub.report("unknown", Readings { power: Some(999.0), ..Default::default() }, 1000);
        assert_eq!(hub.telemetry(false, 1500), Telemetry { power: 215.0, cadence: Some(80.0), hr: Some(150.0), has_power: true });
        // The power meter goes quiet; the trainer keeps reporting.
        hub.report("trainer", Readings { power: Some(190.0), cadence: Some(82.0), ..Default::default() }, 6000);
        assert_eq!(hub.telemetry(false, 6500), Telemetry { power: 190.0, cadence: Some(82.0), hr: None, has_power: true });
        assert_eq!(hub.snapshot(6500)[0].readings.power, Some(190.0));
        assert_eq!(hub.snapshot(6500)[1].readings.power, None);
        assert_eq!(hub.trainer(), Some("trainer"));
        assert!(hub.set_status("trainer", DeviceStatus::Searching));
        assert_eq!(hub.trainer(), None);
        hub.remove("trainer");
        assert_eq!(hub.telemetry(false, 6500), Telemetry { power: 0.0, cadence: None, hr: None, has_power: false });
    }

    #[test]
    fn estimates_power_from_wheel_speed_only_when_asked() {
        let mut hub = Hub::default();
        hub.add(dev("spd", DeviceKind::Speed));
        hub.report("spd", Readings { wheel_speed: Some(8.9408), ..Default::default() }, 0); // 20 mph
        assert!(!hub.telemetry(false, 100).has_power);
        let t = hub.telemetry(true, 100);
        assert!(close(t.power, virtual_power(8.9408), 1e-9));
        assert!(t.power > 240.0 && t.power < 270.0);
    }
}
