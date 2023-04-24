#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_assignments)]


use std::str;
use std::thread;
use std::hash::Hash;
use std::collections::HashSet;
use std::cmp::PartialEq;
use std::time::Duration;
use std::collections::HashMap;
use std::net::{TcpStream, Shutdown};
use std::io::{self, BufRead, BufReader, Write};

use rand::prelude::*;
use rand::distributions::WeightedIndex;

use crate::Protocol; 
use crate::Message;
use crate::MessageSequence;
use crate::StateTransition;
use crate::StateModel;
use crate::Response;

use crate::GreetingProtocol;

pub struct FuzzConfig {
	pub generations: usize,
	pub selection_pressure: f32,
	pub sequence_mutation_rate: f32,
	pub sequence_crossover_rate: f32,
	pub message_mutation_rate: f32,
	pub message_crossover_rate: f32,
	pub message_pool_size: usize,
	pub pool_update_rate: f32,
	pub state_rarity_threshold: f32,
	pub state_coverage_weight: f32,
	pub state_jump_efficiency_weight: f32,
	pub state_rarity_weight: f32,
}


pub struct Client<P: Protocol + Clone + PartialEq> {
	server_address: String,
	protocol: P,
	pub corpus: Vec<MessageSequence<P>>,
	state_model: StateModel<P>,
	message_pool: Vec<Message<P>>, 
}

impl<P: Protocol + Clone + PartialEq> Client<P> {
    // Initialize new client with random corpus and message_pool
    pub fn new(server_address: String, protocol: P) -> Self {
        const MESSAGE_SEQUENCE_LENGTH: usize = 10;
        const MESSAGE_POOL_LENGTH: usize = 10;
        const INITIAL_CORPUS_LENGTH: usize = 10;

        let mut corpus = Vec::new();
        for _ in 0..INITIAL_CORPUS_LENGTH {
            corpus.push(MessageSequence::random_message_sequence(
                protocol.clone(),
                MESSAGE_SEQUENCE_LENGTH,
            ));
        }

        let mut message_pool = Vec::new();
        for _ in 0..MESSAGE_POOL_LENGTH {
            message_pool.push(Message::random_message(protocol.clone()));
        }

        Self {
            server_address,
            protocol,
            corpus,
            state_model: StateModel::new(),
            message_pool,
        }
    }

    fn initialize_stream(&mut self) -> TcpStream {
    	TcpStream::connect(self.server_address.as_str())
    	.expect("Could not connect to server")
    }

    fn terminate_stream(&mut self, stream: TcpStream) {
    	stream.shutdown(Shutdown::Both).expect("Shutdown call failed");
    }

    fn send_message(&mut self, mut stream: &TcpStream, message: &Message<P>) {
    	stream.write(&message.data)
    	.expect("Failed to write to server");
    }

	fn read_response(&mut self, reader: &mut BufReader<&TcpStream>) -> Response {
		let mut buffer: Vec<u8> = Vec::new();
        	reader.read_until(b'\n', &mut buffer).expect("Could not read into buffer");
        	return Response::new(buffer);
	}

	// A new TcpStream is created and destroyed for each MessageSequence
	// Send every MessageSequence in the current corpus and collect the Message sent
	// with the Responses received and return this collection
	fn run_message_sequence(&mut self, message_sequence: &MessageSequence<P>) -> Vec<(Message<P>, Response)> {
		let stream: TcpStream = self.initialize_stream();
		let mut reader = BufReader::new(&stream);
		let mut message_response: Vec<(Message<P>, Response)> = Vec::new();

		for (index, message) in message_sequence.messages.iter().enumerate() {
			self.send_message(&stream, message);

			let response: Response = self.read_response(&mut reader);
			message_response.push((message.clone(), response));

			// wait message_sequence.timings[index] many seconds
			// before sending the next message
			if index < message_sequence.timings.len() {
				let sleep_duration: Duration = Duration::from_secs_f32(message_sequence.timings[index]);
				thread::sleep(sleep_duration);
			}
		}
		self.terminate_stream(stream);
		return message_response
	}

	// Take the interection history (Vec<Message<P>, Response)>) of each MessageSequnce sent and 
	// construct the resultant StateTransitions from this information and return a vector of all the 
	// StateTransitions
	fn process_trace(&mut self, corpus_trace: Vec<Vec<(Message<P>, Response)>>) -> (Vec<StateTransition<P::ServerState, P>>, Vec<usize>) {
	    let mut state_transitions: Vec<StateTransition<P::ServerState, P>> = Vec::new();

	    // This vector will contain a count of the unique ServerStates prompted by each MessageSequence
	    let mut unique_server_state_counts: Vec<usize> = Vec::new();


	    for interaction_history in corpus_trace {
	        // Option is used here to represent the possibility of having
	        // a server state or not since the previous state is unknown
	        // at the beginning of an interaction history.
	        let mut previous_server_state: Option<P::ServerState> = None;

	        // A HashSet is created for each interaction_history
	        let mut unique_server_states: HashSet<P::ServerState> = HashSet::new();

	        for (message, response) in interaction_history {
	            let target_state: P::ServerState = self.protocol.parse_response(&response);

	            // The HashSet automatically deduplicates so we need not worry checking for repeats
	            unique_server_states.insert(target_state.clone());
	            
	            // If previous_server_state is not None, then we can initialize
	            // an instance of StateTransition as we have every field's value
	            if let Some(source_state) = previous_server_state {
	                let state_transition = StateTransition {
	                    source_state,
	                    message: message.clone(),
	                    target_state: target_state.clone(),
	                };
	                state_transitions.push(state_transition);
	            }

	            // The server state prompted by the current message,
	            // i.e, target_state is the next message's previous_server_state
	            previous_server_state = Some(target_state);
	        }

	        // Add the count to the vector
	        unique_server_state_counts.push(unique_server_states.len());
	    }

	    return (state_transitions, unique_server_state_counts);
	}

	// Go through each StateTransition in the processed trace and use them to update state_model
    fn update_state_model(&mut self, state_transitions: Vec<StateTransition<P::ServerState, P>>) {
        for transition in state_transitions {
            self.state_model.add(transition.source_state.clone(), transition.target_state.clone(), &transition.message);
        }
    }

    // Go through each MessageSequence within the corpus and mutate it according to the mutation rate
    fn mutate_corpus(&mut self, message_sequence_mutation_rate: f32, message_mutation_rate: f32) {
        let mut rng = rand::thread_rng();

        for message_sequence in &mut self.corpus {
            if rng.gen::<f32>() < message_sequence_mutation_rate {
                message_sequence.mutate_message_sequence(self.protocol.clone(), message_mutation_rate, &self.message_pool);
            }
        }
    }

    // Perform crossover on the MessageSequences within corpus
    fn crossover_corpus(&mut self, message_sequence_crossover_rate: f32, message_crossover_rate: f32) {
        let mut rng = rand::thread_rng();
        let corpus_len = self.corpus.len();
        
        // Pairs of indices to perform crossover on
        let mut crossover_pairs: Vec<(usize, usize)> = Vec::new();

        // This loop iterates over all i, j in {0,...,corpus_len} such that i < j effectively collecting 
        // all unique ordered pairs to be formed irrespective of ordering in the components
        for i in 0..corpus_len {
            for j in i + 1..corpus_len {
                if rng.gen::<f32>() < message_sequence_crossover_rate {
                    crossover_pairs.push((i, j));
                }
            }
        }

        // Here we replace the parents in the corpus with their two offspring
        for (idx1, idx2) in crossover_pairs {
        	let mut parent1 = self.corpus[idx1].clone();
        	let parent2 = self.corpus[idx2].clone();

            let (offspring1, offspring2) = parent1.crossover_message_sequences(&parent2, message_crossover_rate);
            self.corpus[idx1] = offspring1;
            self.corpus[idx2] = offspring2;
        }
    }

    // This method aims to identify "rare" ServerStates, which are the states that occur less frequently
    // in the StateModel based on the given rarity_threshold which is to denote a percentage.
	fn identify_rare_server_states(&self, rarity_threshold: f32) -> HashSet<P::ServerState> {
		// This will contain the "rare" server states identifies
	    let mut rare_server_states = HashSet::new();

	    // This will contain (key, value) pairs where key is defined as a particular ServerState
	    // and value is the number of times that ServerState has been transitioned to out of all
	    // transitions recorded in the state_model
	    let mut server_state_counts = HashMap::new();

	    // This will be the sum of all the number of occurrances of each ServerState
	    let mut total_server_state_occurrences = 0;

	    // Iterate over each vector of StateTransitions pointed to by each unique ServerState
	    for transitions in self.state_model.inner.values() {
	    	// Iterate over each StateTransition which will contain a "target_state"
	    	// If that target_state is already present in server_state_counts as a key, increment
	    	// the value it points to by 1. Otherwise, insert a new (target_state, occurrances)
	    	// (key, value) pair into the server_state_counts HashMap
	        for state_transition in transitions {
	            *server_state_counts.entry(state_transition.target_state.clone()).or_insert(0) += 1;
	            total_server_state_occurrences += 1;
	        }
	    }

	    // Iternate over all the (ServerState, occurances) pairs in server_state_count
	    for (server_state, count) in server_state_counts {
	    	// If out of the total number of all occurances of all ServerStates, this 
	    	// ServerState has occured less than rarity_threshold percent, then it is a "rare"
	    	// ServerState and will be placed into the rare_server_states HashSet
	        let proportion = count as f32 / total_server_state_occurrences as f32;
	        if proportion < rarity_threshold {
	            rare_server_states.insert(server_state);
	        }
	    }

	    rare_server_states
	}

	// Returns the proportion of unique ServerStates prompted by an individual MessageSequence out of 
	// all unique ServerStates vistted throughout the running of the program
	fn calculate_coverage_score(&self, unique_server_states: usize) -> f32 {
	    let total_unique_server_states_visited = self.state_model.count_unique_server_states();
	    unique_server_states as f32 / total_unique_server_states_visited as f32
	}

	fn calculate_efficiency_score(&mut self, messages: &Vec<Message<P>>) -> f32 {
		todo!();
	}

	fn calculate_rarity_score(&mut self, messages: &Vec<Message<P>>, rare_server_states: &HashSet<P::ServerState>) -> f32 {
		todo!();
	} 

	fn evaluate_fitness(
		&mut self, 
		corpus: &mut Vec<MessageSequence<P>>, 
		unique_server_states_visited: &[usize], 
		rare_server_states: &HashSet<P::ServerState>,
		state_coverage_weight: f32,
		state_jump_efficiency_weight: f32,
		state_rarity_weight: f32,
		) {
	    for (i, message_sequence) in corpus.iter_mut().enumerate() {
	        let coverage_score = self.calculate_coverage_score(unique_server_states_visited[i]);
	        let efficiency_score = self.calculate_efficiency_score(&message_sequence.messages);
	        let rarity_score = self.calculate_rarity_score(&message_sequence.messages, rare_server_states);

	        // Combine the three scores with their respective weights to compute the final fitness
	        let fitness = coverage_score * state_coverage_weight
	            + efficiency_score * state_jump_efficiency_weight
	            + rarity_score * state_rarity_weight;

	        message_sequence.fitness = fitness;
	    }
	}

	pub fn fuzz(&mut self, config: FuzzConfig) {

		for _ in 0..config.generations {
			let corpus_len: usize = self.corpus.len();
			let mut message_sequence: MessageSequence<P>;
			let mut interaction_history: Vec<(Message<P>, Response)>;
			let mut corpus_trace: Vec<Vec<(Message<P>, Response)>> = Vec::new();

			let mut rng = rand::thread_rng();

			for i in 0..corpus_len {
				message_sequence = self.corpus[i].clone();
				interaction_history = self.run_message_sequence(&message_sequence);
				corpus_trace.push(interaction_history);

		        // Update message_pool with a random message from the current message_sequence at the defined rate
    			if rng.gen_range(0.0..1.0) < config.pool_update_rate {
    				if self.message_pool.len() == config.message_pool_size {
    					let random_index = rng.gen_range(0..config.message_pool_size);
    					self.message_pool.remove(random_index);
    				}
        			let random_message_idx = rng.gen_range(0..message_sequence.messages.len());
        			let random_message = message_sequence.messages[random_message_idx].clone();
        			self.message_pool.push(random_message);
    			}
			}

			// Process the the corpus_trace to get the ServerStates needed to update the StateModel
			let (state_transitions, unique_server_states_visited): (Vec<StateTransition<P::ServerState, P>>, Vec<usize>) = self.process_trace(corpus_trace);
			self.update_state_model(state_transitions);

			// Identify rare server states
        	let rare_server_states = self.identify_rare_server_states(config.state_rarity_threshold);
		}
	}
}