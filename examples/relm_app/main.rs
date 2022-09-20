use gtk::prelude::*;
use relm4::{gtk, send, AppUpdate, Model, RelmApp, Sender, WidgetPlus, Widgets};
use tokio::runtime::Runtime;

use radiorust::*;
mod simple_receiver;
use simple_receiver::*;

struct AppModel {
    _rt: Runtime,
    simple_sdr: SimpleSdr,
    volume: f64,
}

enum AppMsg {
    VolumeUp,
    VolumeDown,
    FreqUp,
    FreqDown,
}

impl Model for AppModel {
    type Msg = AppMsg;
    type Widgets = AppWidgets;
    type Components = ();
}

impl AppUpdate for AppModel {
    fn update(&mut self, msg: AppMsg, _components: &(), _sender: Sender<AppMsg>) -> bool {
        match msg {
            AppMsg::VolumeUp => {
                self.volume *= 2.0f64.sqrt();
                let vol = self.volume as f32;
                self.simple_sdr.volume.set_closure(move |x| x * vol);
            }
            AppMsg::VolumeDown => {
                self.volume /= 2.0f64.sqrt();
                let vol = self.volume as f32;
                self.simple_sdr.volume.set_closure(move |x| x * vol);
            }
            AppMsg::FreqUp => {
                let mut shift = self.simple_sdr.freq_shifter.shift();
                shift += 0.1e6;
                self.simple_sdr.freq_shifter.set_shift(shift);
            }
            AppMsg::FreqDown => {
                let mut shift = self.simple_sdr.freq_shifter.shift();
                shift -= 0.1e6;
                self.simple_sdr.freq_shifter.set_shift(shift);
            }
        }
        true
    }
}

#[relm4_macros::widget]
impl Widgets<AppModel, ()> for AppWidgets {
    view! {
        gtk::ApplicationWindow {
            set_title: Some("Radio Rust"),
            set_default_width: 300,
            set_default_height: 100,
            set_child = Some(&gtk::Box) {
                set_orientation: gtk::Orientation::Vertical,
                set_margin_all: 5,
                set_spacing: 5,
                append = &gtk::Button::with_label("VolumeUp") {
                    connect_clicked(sender) => move |_| {
                        send!(sender, AppMsg::VolumeUp);
                    },
                },
                append = &gtk::Button::with_label("VolumeDown") {
                    connect_clicked(sender) => move |_| {
                        send!(sender, AppMsg::VolumeDown);
                    },
                },
                append = &gtk::Button::with_label("FreqUp") {
                    connect_clicked(sender) => move |_| {
                        send!(sender, AppMsg::FreqUp);
                    },
                },
                append = &gtk::Button::with_label("FreqDown") {
                    connect_clicked(sender) => move |_| {
                        send!(sender, AppMsg::FreqDown);
                    },
                },
            },
        }
    }
}

fn main() {
    let rt = Runtime::new().unwrap();
    let enter = rt.enter();
    let simple_sdr = SimpleSdr::new();
    drop(enter);
    let model = AppModel {
        _rt: rt,
        simple_sdr,
        volume: 1.0,
    };
    let app = RelmApp::new(model);
    app.run();
}
