// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use polkadot_sdk::*;

use super::benchmarking::{inherent_benchmark_data, RemarkBuilder, TransferKeepAliveBuilder};
use crate::{
	chain_spec, service,
	service::{new_partial, FullClient},
	Cli, Subcommand,
};
use frame_benchmarking_cli::*;
use kitchensink_runtime::{ExistentialDeposit, RuntimeApi};
use node_primitives::Block;
use sc_cli::{Result, SubstrateCli};
use sc_service::PartialComponents;
use sp_keyring::Sr25519Keyring;
use sp_runtime::traits::HashingFor;
use sc_consensus::{
	block_import::{BlockImportParams, ForkChoiceStrategy},
	BlockImport, StateAction,
};
use sp_consensus::BlockOrigin;
use sc_block_builder::{BlockBuilderBuilder, BuiltBlock};
use crate::service::{create_extrinsic_with};
use kitchensink_runtime::{constants::currency::*, BalancesCall};
use sp_runtime::{
	generic,
	transaction_validity::{InvalidTransaction, TransactionValidityError},
	AccountId32, MultiAddress, OpaqueExtrinsic,
};
use sp_blockchain::HeaderBackend;
use sp_api::ProvideRuntimeApi;
use sp_api::CallApiAt;
use sp_api::ApiExt;
use sc_block_builder::BlockBuilderApi;

use std::sync::Arc;

impl SubstrateCli for Cli {
	fn impl_name() -> String {
		"Substrate Node".into()
	}

	fn impl_version() -> String {
		env!("SUBSTRATE_CLI_IMPL_VERSION").into()
	}

	fn description() -> String {
		env!("CARGO_PKG_DESCRIPTION").into()
	}

	fn author() -> String {
		env!("CARGO_PKG_AUTHORS").into()
	}

	fn support_url() -> String {
		"https://github.com/paritytech/polkadot-sdk/issues/new".into()
	}

	fn copyright_start_year() -> i32 {
		2017
	}

	fn load_spec(&self, id: &str) -> std::result::Result<Box<dyn sc_service::ChainSpec>, String> {
		let spec = match id {
			"" =>
				return Err(
					"Please specify which chain you want to run, e.g. --dev or --chain=local"
						.into(),
				),
			"dev" => Box::new(chain_spec::development_config()),
			"local" => Box::new(chain_spec::local_testnet_config()),
			"fir" | "flaming-fir" => Box::new(chain_spec::flaming_fir_config()?),
			"staging" => Box::new(chain_spec::staging_testnet_config()),
			path =>
				Box::new(chain_spec::ChainSpec::from_json_file(std::path::PathBuf::from(path))?),
		};
		Ok(spec)
	}
}

/// Parse command line arguments into service configuration.
pub fn run() -> Result<()> {
	let cli = Cli::from_args();

	match &cli.subcommand {
		None => {
			let runner = cli.create_runner(&cli.run)?;
			runner.run_node_until_exit(|config| async move {
				service::new_full(config, cli).map_err(sc_cli::Error::Service)
			})
		},
		Some(Subcommand::Inspect(cmd)) => {
			let runner = cli.create_runner(cmd)?;

			runner.sync_run(|config| cmd.run::<Block, RuntimeApi>(config))
		},
		Some(Subcommand::Benchmark(cmd)) => {
			let runner = cli.create_runner(cmd)?;

			runner.sync_run(|config| {
				// This switch needs to be in the client, since the client decides
				// which sub-commands it wants to support.
				match cmd {
					BenchmarkCmd::Pallet(cmd) => {
						if !cfg!(feature = "runtime-benchmarks") {
							return Err(
								"Runtime benchmarking wasn't enabled when building the node. \
							You can enable it with `--features runtime-benchmarks`."
									.into(),
							)
						}

						cmd.run_with_spec::<HashingFor<Block>, sp_statement_store::runtime_api::HostFunctions>(Some(config.chain_spec))
					},
					BenchmarkCmd::Block(cmd) => {
						// ensure that we keep the task manager alive
						let partial = new_partial(&config, None)?;
						cmd.run(partial.client)
					},
					#[cfg(not(feature = "runtime-benchmarks"))]
					BenchmarkCmd::Storage(_) => Err(
						"Storage benchmarking can be enabled with `--features runtime-benchmarks`."
							.into(),
					),
					#[cfg(feature = "runtime-benchmarks")]
					BenchmarkCmd::Storage(cmd) => {
						// ensure that we keep the task manager alive
						let partial = new_partial(&config, None)?;
						let db = partial.backend.expose_db();
						let storage = partial.backend.expose_storage();

						cmd.run(config, partial.client, db, storage)
					},
					BenchmarkCmd::Overhead(cmd) => {
						// ensure that we keep the task manager alive
						let partial = new_partial(&config, None)?;
						let ext_builder = RemarkBuilder::new(partial.client.clone());

						cmd.run(
							config.chain_spec.name().into(),
							partial.client,
							inherent_benchmark_data()?,
							Vec::new(),
							&ext_builder,
							false,
						)
					},
					BenchmarkCmd::Extrinsic(cmd) => {
						// ensure that we keep the task manager alive
						let partial = service::new_partial(&config, None)?;
						// Register the *Remark* and *TKA* builders.
						let ext_factory = ExtrinsicFactory(vec![
							Box::new(RemarkBuilder::new(partial.client.clone())),
							Box::new(TransferKeepAliveBuilder::new(
								partial.client.clone(),
								Sr25519Keyring::Alice.to_account_id(),
								ExistentialDeposit::get(),
							)),
						]);

						cmd.run(
							partial.client,
							inherent_benchmark_data()?,
							Vec::new(),
							&ext_factory,
						)
					},
					BenchmarkCmd::Machine(cmd) =>
						cmd.run(&config, SUBSTRATE_REFERENCE_HARDWARE.clone()),
				}
			})
		},
		Some(Subcommand::Key(cmd)) => cmd.run(&cli),
		Some(Subcommand::Sign(cmd)) => cmd.run(),
		Some(Subcommand::Verify(cmd)) => cmd.run(),
		Some(Subcommand::Vanity(cmd)) => cmd.run(),
		Some(Subcommand::BuildSpec(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.sync_run(|config| cmd.run(config.chain_spec, config.network))
		},
		Some(Subcommand::CheckBlock(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.async_run(|config| {
				let PartialComponents { client, task_manager, import_queue, .. } =
					new_partial(&config, None)?;
				Ok((cmd.run(client, import_queue), task_manager))
			})
		},
		Some(Subcommand::ExportBlocks(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.async_run(|config| {
				let PartialComponents { client, task_manager, .. } = new_partial(&config, None)?;
				Ok((cmd.run(client, config.database), task_manager))
			})
		},
		Some(Subcommand::ExportState(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.async_run(|config| {
				let PartialComponents { client, task_manager, .. } = new_partial(&config, None)?;
				Ok((cmd.run(client, config.chain_spec), task_manager))
			})
		},
		Some(Subcommand::ImportBlocks(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.async_run(|config| {
				let PartialComponents { client, task_manager, import_queue, .. } =
					new_partial(&config, None)?;
				Ok((cmd.run(client, import_queue), task_manager))
			})
		},
		Some(Subcommand::PurgeChain(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.sync_run(|config| cmd.run(config.database))
		},
		Some(Subcommand::Revert(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.async_run(|config| {
				let PartialComponents { client, task_manager, backend, .. } =
					new_partial(&config, None)?;
				let aux_revert = Box::new(|client: Arc<FullClient>, backend, blocks| {
					sc_consensus_babe::revert(client.clone(), backend, blocks)?;
					sc_consensus_grandpa::revert(client, blocks)?;
					Ok(())
				});
				Ok((cmd.run(client, backend, Some(aux_revert)), task_manager))
			})
		},
		Some(Subcommand::ChainInfo(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.sync_run(|config| cmd.run::<Block>(&config))
		},
		Some(Subcommand::BenchmarkBlockExecution(cmd)) => {
			let runner = cli.create_runner(cmd)?;
			runner.sync_run(|mut config| {
				let PartialComponents { client, task_manager, import_queue, .. } =
					new_partial(&config, None)?;
				run_block_execution(cmd, client);
				Ok(())
			})
		},
	}
}

const MINIMUM_PERIOD_FOR_BLOCKS: u64 = 1500;

	pub fn run_block_execution<C>(cmd: &crate::BlockExecutionCmd, client: Arc<C>)
	where
		C: sc_consensus::BlockImport<Block>,
		C: HeaderBackend<Block>
			+ ProvideRuntimeApi<Block>
			+ CallApiAt<Block>
			+ Send
			+ Sync
			+ 'static,
		C::Api: ApiExt<Block> + BlockBuilderApi<Block> + 'static,
	{
		let genesis_hash = client.info().genesis_hash;
		let block_hash = client.info().best_hash;
		let block_number = client.info().best_number;

		// Creating those is surprisingly costly, so let's only do it once and later just `clone` them.
		let src = Sr25519Keyring::Alice.pair();
		let dst: MultiAddress<AccountId32, u32> = Sr25519Keyring::Bob.to_account_id().into();

		// Add as many transfer extrinsics as possible into a single block.
		let mut extrinsics = vec![extrinsic_set_time(1)];
		for nonce in 0..cmd.number {
			extrinsics.push(
				create_extrinsic_with(
					src.clone(),
					BalancesCall::transfer_allow_death { dest: dst.clone(), value: 1 * DOLLARS },
					nonce as u32,
					block_number,
					block_hash,
					genesis_hash,
				)
				.into()
			);
		}

		let start = std::time::Instant::now();
		let mut block_builder = BlockBuilderBuilder::new(client.as_ref())
			.on_parent_block(block_hash)
			.with_parent_block_number(block_number)
			.build()
			.unwrap();
		let mut took = start.elapsed().as_millis();
		println!("Initialized block of {} transfers took {}ms", cmd.number, took);
		for tx in extrinsics {
			block_builder.push(tx).unwrap();
		}
		took = start.elapsed().as_millis() - took;
		println!("Executed block of {} transfers took {}ms", cmd.number, took);
		let block = block_builder.build().unwrap();
		took = start.elapsed().as_millis() - took;
		println!("Finalized block of {} transfers took {}ms", cmd.number, took);
		import_block(&client, block);
		took = start.elapsed().as_millis() - took;
		println!("Imported block of {} transfers took {}ms", cmd.number, took);

		let block_hash = client.info().best_hash;
		let block_number = client.info().best_number;
		let start = std::time::Instant::now();
		let mut block_builder = BlockBuilderBuilder::new(client.as_ref())
			.on_parent_block(block_hash)
			.with_parent_block_number(block_number)
			.build()
			.unwrap();
		block_builder.push(extrinsic_set_time(1 + MINIMUM_PERIOD_FOR_BLOCKS)).unwrap();
		import_block(&client, block_builder.build().unwrap());
		println!("The next block of {} transfers took {}ms", cmd.number, start.elapsed().as_millis());
	}


fn extrinsic_set_time(now: u64) -> OpaqueExtrinsic {
	let utx: kitchensink_runtime::UncheckedExtrinsic = generic::UncheckedExtrinsic::new_bare(
		kitchensink_runtime::RuntimeCall::Timestamp(pallet_timestamp::Call::set { now }),
	)
	.into();
	utx.into()
}

fn import_block<C: sc_consensus::BlockImport<Block>>(client: &Arc<C>, built: BuiltBlock<Block>) {
	let mut params = BlockImportParams::new(BlockOrigin::File, built.block.header);
	params.state_action =
		StateAction::ApplyChanges(sc_consensus::StorageChanges::Changes(built.storage_changes));
	params.fork_choice = Some(ForkChoiceStrategy::Custom(true));
	futures::executor::block_on(client.import_block(params))
		.expect("importing a block doesn't fail");
}
