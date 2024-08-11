use std::{
    collections::VecDeque,
    sync::{mpsc::Receiver, Mutex},
};
use candle_core::Tensor;
use tracing::field::debug;

use crate::{
    get_mut_arcmutex,
    pipeline::Pipeline,
    request::{Request, Sequence},
    response::Response,
    scheduler::{
        Scheduler,
        SchedulerOutput,
    },
};

pub struct Engine {
    rx: Receiver<Request>,
    pipeline: Box<Mutex<dyn Pipeline>>,
    requests: VecDeque<Request>,
    scheduler: Scheduler<VecDeque<Sequence>>,
    id: usize,
}

impl Engine {
    pub fn new(rx: Receiver<Request>, pipeline: Box<Mutex<dyn Pipeline>>) -> Self {
        Self {
            rx,
            pipeline,
            requests: VecDeque::new(),
            scheduler: Scheduler::new(),
            id: 0,
        }
    }

    pub fn run(&mut self) {
        loop {
            if let Ok(request) = self.rx.recv() {
                self.add_request(request);
            }
            let mut scheduled = self.scheduler.schedule();
            self.clone_in_cache(&scheduled);
            let _logits = get_mut_arcmutex!(self.pipeline)
                .forward(scheduled.seqs.iter_mut().collect::<Vec<_>>());
            self.clone_out_cache(&scheduled);
        }
    }

    fn clone_in_cache(&self, scheduled: &SchedulerOutput) {
        let mut new_cache = Vec::new();
        for layer in 0..get_mut_arcmutex!(self.pipeline).num_hidden_layers() {
            let mut k_vec = Vec::new();
            let mut v_vec = Vec::new();
            for seq in &scheduled.seqs {
                let cache = seq.cache().lock();
                let cache = cache.get(layer).unwrap();

                let cache = cache.as_ref().unwrap();
                k_vec.push(cache.0.clone().unsqueeze(0).unwrap());
                v_vec.push(cache.1.clone().unsqueeze(0).unwrap());
            }
            new_cache.push(Some((
                Tensor::cat(&k_vec, 0).unwrap(),
                Tensor::cat(&v_vec, 0).unwrap(),
            )));
        }
        *get_mut_arcmutex!(self.pipeline).cache().lock() = new_cache;
    }

    fn clone_out_cache(&self, scheduled: &SchedulerOutput) {
        for layer in 0..get_mut_arcmutex!(self.pipeline).num_hidden_layers() {
            let pipeline = get_mut_arcmutex!(self.pipeline);
            let cache = pipeline.cache().lock();
            let cache = cache.get(layer).unwrap();
            let k_cache = cache.as_ref().unwrap().0.clone();
            let v_cache = cache.as_ref().unwrap().1.clone();

            let k_caches = k_cache.chunk(scheduled.seqs.len(), 0).unwrap();
            debug_assert_eq!(k_caches.len(), scheduled.seqs.len());
            let v_caches = v_cache.chunk(scheduled.seqs.len(), 0).unwrap();
            debug_assert_eq!(v_caches.len(), scheduled.seqs.len());

            for (seq_i, seq) in scheduled.seqs.iter().enumerate() {
                let mut seq_cache = seq.cache().lock();
                let seq_cache = seq_cache.get_mut(layer).unwrap();
                *seq_cache = Some((
                    k_caches.get(seq_i).unwrap().clone(), 
                    v_caches.get(seq_i).unwrap().clone(),
                ));
            }
        }
    }

    fn add_request(&mut self, request: Request) {
        let prompt = match get_mut_arcmutex!(self.pipeline).tokenize_prompt(&request.prompt) {
            Ok(prompt) => prompt,
            Err(e) => {
                // Unwrap reasoning: The reciever should really be there, otherwise it is their fault.
                request.response.send(Response::Error(e.into())).unwrap();
                return;
            }
        };
        let seq = Sequence::new_waiting(
            prompt, 
            self.id,
            get_mut_arcmutex!(self.pipeline).num_hidden_layers(),
        );
        self.id += 1;

        self.requests.push_back(request);
        self.scheduler.add_seq(seq);
    }
}
