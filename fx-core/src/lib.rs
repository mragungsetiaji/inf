
use std::{
    sync::{
        mpsc::{channel, Sender},
        Arc, Mutex,
    },
    thread,
};
use engine::Engine;
use pipeline::Pipeline;

mod models;
mod pipeline;
mod utils;
mod scheduler;
mod response;
mod request;
mod engine;

pub use pipeline::{Loader, MistralLoader, MistralSpecificConfig, TokenSource};
use request::Request;

pub struct FxServ {
    sender: Sender<Request>
}

impl FxServ {
    pub fn new(pipeline: Box<Mutex<dyn Pipeline>>) -> Arc<Self> {
        let (tx, rx) = channel();
        let this = Arc::new(Self {
            sender: tx,
        });

        thread::spawn(move || {
            let mut engine = Engine::new(rx, pipeline);
            engine.run();
        });

        this
    }

    pub fn get_sender(&self) -> Sender<Request> {
        self.sender.clone()
    }
}
