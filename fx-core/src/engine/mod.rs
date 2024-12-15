use std::{
    cell::RefCell,
    collections::VecDeque,
    iter::zip,
    rc::Rc,
    sync::{mpsc::Receiver, Mutex},
};

use candle_core::Tensor;
use candle_sampling::logits_processor::{LogitsProcessor, Logprobs, SamplingMethod};

use crate::{
    deref_mut_refcell, deref_refcell, get_mut_arcmutex, handle_seq_error,
    pipeline::Pipeline,
    request::{Request, Sequence, SequenceState},
    response::Response,
    scheduler::{Scheduler, SchedulerOutput},
};

const SEED: u64 = 0;

pub struct Engine {
    rx: Receiver<Request>,
    pipeline: Box<Mutex<dyn Pipeline>>,
    requests: VecDeque<Request>,
    scheduler: Scheduler<VecDeque<Rc<RefCell<Sequence>>>>,
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
            let scheduled = self.scheduler.schedule();
            self.clone_in_cache(&scheduled);
            let logits = get_mut_arcmutex!(self.pipeline).forward(scheduled.seqs.clone());
            self.clone_out_cache(&scheduled);

            // TODO: Unwrapping is certainly incorrect. Must handle error
            let logits = logits.unwrap();
            let seqs_len = scheduled.seqs.len();
            let logits_seq = logits.chunk(seqs_len, 0).unwrap();
            debug_assert_eq!(logits_seq.len(), seqs_len);
            for (logits_per_seq, seq) in zip(logits_seq, scheduled.seqs.iter()) {
                let next_token: Logprobs = handle_seq_error!(
                    get_mut_arcmutex!(self.pipeline).sample(logits_per_seq, seq.clone()),
                    deref_refcell!(seq).responder()
                );
                let next_token = next_token.token as u32;
                deref_mut_refcell!(seq).add_token(next_token);
                if let Some(state) = deref_refcell!(seq)
                    .is_done(next_token, get_mut_arcmutex!(self.pipeline).eos_tok())
                {
                    deref_mut_refcell!(seq).set_state(SequenceState::Done(state));
                }
            }
        }
    }

    fn clone_in_cache(&self, scheduled: &SchedulerOutput) {
        let mut new_cache = Vec::new();
        for layer in 0..get_mut_arcmutex!(self.pipeline).num_hidden_layers() {
            let mut k_vec = Vec::new();
            let mut v_vec = Vec::new();
            for seq in scheduled.seqs.iter() {
                let seq = deref_refcell!(seq);
                let cache = seq.cache().lock();
                let cache = cache.get(layer).unwrap();
                // TODO: Unwrapping is certainly incorrect. Need to find a way to handle some caches with values, some without.
                let cache = cache.as_ref().unwrap();
                k_vec.push(cache.0.clone().unsqueeze(0).unwrap());
                v_vec.push(cache.1.clone().unsqueeze(0).unwrap());
            }
            // NOTE Unwrap reasoning: We have the correct dims
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
                let seq = deref_refcell!(seq);
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
        let prompt = handle_seq_error!(
            get_mut_arcmutex!(self.pipeline).tokenize_prompt(&request.prompt),
            request.response
        );
        let sampling_method = match (request.sampling_params.top_k, request.sampling_params.top_p) {
            (Some(topk), None) => SamplingMethod::TopK(topk),
            (None, Some(topp)) => SamplingMethod::TopP(topp),
            (None, None) | (Some(_), Some(_)) => {
                // NOTE Unwrap reasoning: The reciever should really be there, otherwise it is their fault.
                request
                    .response
                    .send(Response::Error(
                        "Please specify either topk or topp.".into(),
                    ))
                    .unwrap();
                return;
            }
        };
        let seq = Sequence::new_waiting(
            prompt,
            self.id,
            get_mut_arcmutex!(self.pipeline).num_hidden_layers(),
            request.response.clone(),
            LogitsProcessor::new(
                SEED,
                request.sampling_params.temperature,
                sampling_method,
                request.sampling_params.top_n_logprobs,
                get_mut_arcmutex!(self.pipeline).tokenizer(),
                request.sampling_params.repeat_penalty,
            ),
            request
                .sampling_params
                .stop_toks
                .clone()
                .unwrap_or_default(),
        );
        self.id += 1;

        self.requests.push_back(request);
        self.scheduler.add_seq(seq);
    }
}
