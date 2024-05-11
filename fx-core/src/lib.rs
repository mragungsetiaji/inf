
use std::{
    collections::VecDeque,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc,
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
    pipeline: Arc<dyn Pipeline>,
    sender: Sender<Request>
}

impl FxServ {
    pub fn new(pipeline: Arc<dyn Pipeline>) -> Arc<Self> {
        let (tx, rx) = channel();
        let pipeline_clone = pipeline.clone();

        let this = Arc::new(Self {
            pipeline,
            sender: tx,
        });

        thread::spawn(move || {
            let mut engine = Engine::new(rx, pipeline_clone);
            engine.run();
        });

        this
    }

    pub fn get_sender(&self) -> Sender<Request> {
        self.sender.clone()
    }
}
