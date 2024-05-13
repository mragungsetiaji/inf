use std::{
    collections::VecDeque,
    iter::repeat,
    sync::{mpsc::Receiver, Arc},
};
use anyhow::Result;
use candle_core::{Device, Tensor};
use crate::{
    pipeline::Pipeline,
    request::{Request, Sequence},
    response::Response,
    scheduler::Scheduler,
};

pub struct Engine {
    rx: Receiver<Request>,
    pipeline: Arc<dyn Pipeline>,
    requests: VecDeque<Request>,
    scheduler: Scheduler<VecDeque<Sequence>>,
    id: usize,
}

impl Engine {
    pub fn new(rx: Receiver<Request>, pipeline: Arc<dyn Pipeline>) -> Self {
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
            let scheduled = self.scheduler.schedule();
        }
    }

    fn prepare_inputs(&self, seqs: Vec<Sequence>, device: &Device) -> Tensor {
        let max_len = seqs.iter().map(
            |seq| seq.len()
        ).max().unwrap();
        let padding_tok = 0;
        let mut seqs_tensors = Vec::new();
        for seq in seqs.into_iter() {
            let mut toks = seq.get_tokens().to_vec();
            toks.extend(repeat(padding_tok).take(max_len - seq.len()));
            seqs_tensors.push(
                Tensor::from_vec(toks, max_len, device)
                    .unwrap()
                    .unsqueeze(0)
                    .unwrap(),
            );
        }
        Tensor::cat(&seqs_tensors, 0).unwrap()
    }

    fn add_request(&mut self, request: Request) {
        let prompt = match self.pipeline.tokenize_prompt(&request.prompt) {
            Ok(prompt) => prompt,
            Err(e) => {
                // Unwrap reasoning: The reciever should really be there, otherwise it is their fault.
                request.response.send(Response::Error(e.into())).unwrap();
                return;
            }
        };
        let seq = Sequence::new_waiting(prompt, self.id);
        self.id += 1;

        self.requests.push_back(request);
        self.scheduler.add_seq(seq);
    }
}
