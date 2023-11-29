use source_map::{SourceId, Span, SpanWithSource};
use std::{collections::HashMap, iter, vec};

use crate::{
	behavior::{
		constant_functions::{call_constant_function, ConstantFunctionError, ConstantOutput},
		functions::{FunctionBehavior, ThisValue},
	},
	context::{
		calling::CheckThings, get_value_of_variable, CallCheckingBehavior, Environment, Logical,
	},
	diagnostics::{TypeCheckError, TypeStringRepresentation},
	events::{apply_event, Event, RootReference},
	subtyping::{type_is_subtype, BasicEquality, NonEqualityReason, SubTypeResult},
	types::{
		functions::SynthesisedArgument, poly_types::generic_type_arguments::TypeArgumentStore,
		substitute,
	},
	types::{FunctionType, Type},
	FunctionId, SpecialExpressions, TypeId,
};

use super::{
	poly_types::{
		generic_type_arguments::StructureGenericArguments, FunctionTypeArguments, SeedingContext,
	},
	Constructor, PolyNature, StructureGenerics, TypeStore,
};

pub struct CallingInput {
	pub called_with_new: CalledWithNew,
	pub this_value: ThisValue,
	pub call_site_type_arguments: Option<Vec<(TypeId, SpanWithSource)>>,
	pub call_site: SpanWithSource,
}

pub struct CallingInputWithoutThis {
	pub called_with_new: CalledWithNew,
	pub call_site_type_arguments: Option<Vec<(TypeId, SpanWithSource)>>,
	pub call_site: SpanWithSource,
}

pub fn call_type_handle_errors<T: crate::ReadFromFS, M: crate::ASTImplementation>(
	ty: TypeId,
	CallingInput { called_with_new, this_value, call_site_type_arguments, call_site }: CallingInput,
	environment: &mut Environment,
	arguments: Vec<SynthesisedArgument>,
	checking_data: &mut crate::CheckingData<T, M>,
) -> (TypeId, Option<SpecialExpressions>) {
	let result = call_type(
		ty,
		CallingInput {
			called_with_new,
			this_value,
			call_site_type_arguments,
			call_site: call_site.clone(),
		},
		arguments,
		environment,
		&mut CheckThings,
		&mut checking_data.types,
	);
	match result {
		Ok(FunctionCallResult { returned_type, warnings, called, special }) => {
			for warning in warnings {
				checking_data.diagnostics_container.add_info(
					crate::diagnostics::Diagnostic::Position {
						reason: warning.0,
						position: call_site.clone(),
						kind: crate::diagnostics::DiagnosticKind::Info,
					},
				);
			}

			if let Some(called) = called {
				checking_data.type_mappings.called_functions.insert(called);
			}
			(returned_type, special)
		}
		Err(errors) => {
			for error in errors {
				checking_data
					.diagnostics_container
					.add_error(TypeCheckError::FunctionCallingError(error));
			}
			(TypeId::ERROR_TYPE, None)
		}
	}
}

/// TODO this and aliases kindof broken
pub(crate) fn call_type<E: CallCheckingBehavior>(
	on: TypeId,
	CallingInput { called_with_new, this_value, call_site_type_arguments, call_site }: CallingInput,
	arguments: Vec<SynthesisedArgument>,
	environment: &mut Environment,
	behavior: &mut E,
	types: &mut TypeStore,
) -> Result<FunctionCallResult, Vec<FunctionCallingError>> {
	if on == TypeId::ERROR_TYPE
		|| arguments.iter().any(|arg| match arg {
			SynthesisedArgument::NonSpread { ty, .. } => *ty == TypeId::ERROR_TYPE,
		}) {
		return Ok(FunctionCallResult {
			called: None,
			returned_type: TypeId::ERROR_TYPE,
			warnings: Vec::new(),
			special: None,
		});
	}

	if let Some(constraint) = environment.get_poly_base(on, types) {
		create_generic_function_call(
			constraint,
			CallingInput { called_with_new, this_value, call_site_type_arguments, call_site },
			arguments,
			on,
			environment,
			behavior,
			types,
		)
	} else {
		// pub enum Callable {
		// 	Function()
		// }

		let callable = get_logical_callable_from_type(on, types);

		if let Some(logical) = callable {
			let structure_generics = None;
			call_logical(
				logical,
				types,
				CallingInputWithoutThis { called_with_new, call_site_type_arguments, call_site },
				structure_generics,
				arguments,
				environment,
				behavior,
			)
		} else {
			Err(vec![FunctionCallingError::NotCallable {
				calling: crate::diagnostics::TypeStringRepresentation::from_type_id(
					on,
					&environment.as_general_context(),
					types,
					false,
				),
				call_site,
			}])
		}
	}
}

fn call_logical<E: CallCheckingBehavior>(
	logical: Logical<(FunctionId, ThisValue)>,
	types: &mut TypeStore,
	CallingInputWithoutThis { called_with_new, call_site_type_arguments, call_site }: CallingInputWithoutThis,
	structure_generics: Option<StructureGenericArguments>,
	arguments: Vec<SynthesisedArgument>,
	environment: &mut Environment,
	behavior: &mut E,
) -> Result<FunctionCallResult, Vec<FunctionCallingError>> {
	match logical {
		Logical::Pure((func, this_value)) => {
			if let Some(function_type) = types.functions.get(&func) {
				function_type.clone().call(
					CallingInput {
						called_with_new,
						this_value,
						call_site_type_arguments,
						call_site,
					},
					structure_generics,
					&arguments,
					environment,
					behavior,
					types,
					true,
				)
			} else {
				todo!("recursive function type")
			}
		}
		Logical::Or { left, right } => todo!(),
		Logical::Implies { on, antecedent } => call_logical(
			*on,
			types,
			CallingInputWithoutThis { called_with_new, call_site_type_arguments, call_site },
			Some(antecedent),
			arguments,
			environment,
			behavior,
		),
	}
}

fn get_logical_callable_from_type(
	on: TypeId,
	types: &TypeStore,
) -> Option<Logical<(FunctionId, ThisValue)>> {
	match types.get_type_by_id(on) {
		Type::And(_, _) => todo!(),
		Type::Or(left, right) => {
			if let (Some(left), Some(right)) = (
				get_logical_callable_from_type(*left, types),
				get_logical_callable_from_type(*right, types),
			) {
				Some(Logical::Or { left: Box::new(left), right: Box::new(right) })
			} else {
				None
			}
		}
		Type::Constructor(Constructor::StructureGenerics(generic)) => {
			get_logical_callable_from_type(generic.on, types).map(|res| Logical::Implies {
				on: Box::new(res),
				antecedent: generic.arguments.clone(),
			})
		}
		Type::RootPolyType(_) | Type::Constructor(_) => todo!(),

		Type::AliasTo { to, name, parameters } => {
			if parameters.is_some() {
				todo!()
			}
			get_logical_callable_from_type(*to, types)
		}
		Type::NamedRooted { .. } | Type::Constant(_) | Type::Object(_) => None,
		Type::FunctionReference(f, t) | Type::Function(f, t) => Some(Logical::Pure((*f, *t))),
		Type::SpecialObject(_) => todo!(),
	}
}

fn create_generic_function_call<E: CallCheckingBehavior>(
	constraint: TypeId,
	CallingInput { called_with_new, this_value, call_site_type_arguments, call_site }: CallingInput,
	arguments: Vec<SynthesisedArgument>,
	on: TypeId,
	environment: &mut Environment,
	behavior: &mut E,
	types: &mut TypeStore,
) -> Result<FunctionCallResult, Vec<FunctionCallingError>> {
	// TODO don't like how it is mixed
	let result = call_type(
		constraint,
		CallingInput {
			called_with_new,
			this_value,
			call_site_type_arguments,
			call_site: call_site.clone(),
		},
		// TODO clone
		arguments.clone(),
		environment,
		behavior,
		types,
	)?;

	let with = arguments.into_boxed_slice();

	// TODO work this out
	let is_open_poly = false;

	let reflects_dependency = if is_open_poly {
		None
	} else {
		// Skip constant types
		if matches!(result.returned_type, TypeId::UNDEFINED_TYPE | TypeId::NULL_TYPE)
			|| matches!(
				types.get_type_by_id(result.returned_type),
				Type::Constant(..)
					| Type::Object(super::ObjectNature::RealDeal)
					| Type::SpecialObject(..)
			) {
			// TODO nearest fact
			environment.facts.events.push(Event::CallsType {
				on,
				with,
				timing: crate::events::CallingTiming::Synchronous,
				called_with_new, // Don't care about output.
				reflects_dependency: None,
				position: call_site.clone(),
			});

			return Ok(result);
		}

		let constructor = Constructor::FunctionResult {
			// TODO on or to
			on,
			with: with.clone(),
			// TODO unwrap
			result: result.returned_type,
		};

		let constructor_return = types.register_type(Type::Constructor(constructor));

		Some(constructor_return)
	};

	// TODO nearest fact
	environment.facts.events.push(Event::CallsType {
		on,
		with,
		timing: crate::events::CallingTiming::Synchronous,
		called_with_new,
		reflects_dependency,
		position: call_site.clone(),
	});

	// TODO should wrap result in open poly
	Ok(FunctionCallResult {
		called: result.called,
		returned_type: reflects_dependency.unwrap_or(result.returned_type),
		warnings: result.warnings,
		special: None,
	})
}

/// Errors from trying to call a function
pub enum FunctionCallingError {
	InvalidArgumentType {
		parameter_type: TypeStringRepresentation,
		argument_type: TypeStringRepresentation,
		argument_position: SpanWithSource,
		parameter_position: SpanWithSource,
		restriction: Option<(SpanWithSource, TypeStringRepresentation)>,
	},
	MissingArgument {
		parameter_position: SpanWithSource,
		call_site: SpanWithSource,
	},
	ExcessArguments {
		count: usize,
		position: SpanWithSource,
	},
	NotCallable {
		calling: TypeStringRepresentation,
		call_site: SpanWithSource,
	},
	ReferenceRestrictionDoesNotMatch {
		reference: RootReference,
		requirement: TypeStringRepresentation,
		found: TypeStringRepresentation,
	},
	CyclicRecursion(FunctionId, SpanWithSource),
	NoLogicForIdentifier(String, SpanWithSource),
	NeedsToBeCalledWithNewKeyword(SpanWithSource),
}

pub struct InfoDiagnostic(pub String);

/// TODO *result* name bad
pub struct FunctionCallResult {
	pub called: Option<FunctionId>,
	pub returned_type: TypeId,
	// TODO
	pub warnings: Vec<InfoDiagnostic>,
	pub special: Option<SpecialExpressions>,
}

#[derive(Debug, Default, Clone, Copy, binary_serialize_derive::BinarySerializable)]
pub enum CalledWithNew {
	New {
		// [See new.target](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Operators/new.target), which contains a reference to what called new. This does not == this_type
		on: TypeId,
	},
	SpecialSuperCall {
		this_type: TypeId,
	},
	#[default]
	None,
}

impl FunctionType {
	/// Calls the function
	///
	/// Returns warnings and errors
	// Move references in a wrapping struct can be hard due to lifetimes
	#[allow(clippy::too_many_arguments)]
	pub(crate) fn call<E: CallCheckingBehavior>(
		&self,
		CallingInput { called_with_new, mut this_value, call_site_type_arguments, call_site }: CallingInput,
		parent_type_arguments: Option<StructureGenericArguments>,
		arguments: &[SynthesisedArgument],
		environment: &mut Environment,
		behavior: &mut E,
		types: &mut crate::TypeStore,
		// This fixes recursive case of call_function
		call_constant: bool,
	) -> Result<FunctionCallResult, Vec<FunctionCallingError>> {
		// TODO check that parameters vary
		if behavior.in_recursive_cycle(self.id) {
			crate::utils::notify!("Encountered recursion");
			return Ok(FunctionCallResult {
				called: Some(self.id),
				returned_type: TypeId::ERROR_TYPE,
				warnings: Vec::new(),
				// TODO ?
				special: None,
			});
			// let reason = FunctionCallingError::Recursed(self.id, call_site);
			// return Err(vec![reason])
		}

		if let (Some(const_fn_ident), true) = (self.constant_function.as_deref(), call_constant) {
			let has_dependent_argument = arguments.iter().any(|arg| {
				types.get_type_by_id(arg.to_type().expect("dependent spread types")).is_dependent()
			});

			// TODO temp, need a better solution
			let call_anyway = matches!(
				const_fn_ident,
				"debug_type"
					| "print_type" | "debug_effects"
					| "satisfies" | "is_dependent"
					| "bind" | "create_proxy"
			);

			// Most of the time don't call using constant function if an argument is dependent.
			// But sometimes do for the cases above
			if call_anyway || !has_dependent_argument {
				// TODO event
				let result = call_constant_function(
					const_fn_ident,
					this_value,
					&call_site_type_arguments,
					arguments,
					types,
					environment,
				);

				match result {
					Ok(ConstantOutput::Value(value)) => {
						// Very special
						let special = if const_fn_ident == "compile_type_to_object" {
							Some(SpecialExpressions::CompileOut)
						} else {
							None
						};
						return Ok(FunctionCallResult {
							returned_type: value,
							warnings: Default::default(),
							called: None,
							special,
						});
					}
					Ok(ConstantOutput::Diagnostic(diagnostic)) => {
						return Ok(FunctionCallResult {
							returned_type: TypeId::UNDEFINED_TYPE,
							warnings: vec![InfoDiagnostic(diagnostic)],
							called: None,
							// TODO!!
							special: Some(SpecialExpressions::Marker),
						});
					}
					Err(ConstantFunctionError::NoLogicForIdentifier(name)) => {
						let item = FunctionCallingError::NoLogicForIdentifier(name, call_site);
						return Err(vec![item]);
					}
					Err(ConstantFunctionError::BadCall) => {
						crate::utils::notify!(
							"Constant function calling failed, not constant params"
						);
					}
				}
			} else if has_dependent_argument {
				let with = arguments.to_vec().into_boxed_slice();
				// TODO with cloned!!
				let result = self
					.call(
						CallingInput {
							called_with_new,
							this_value,
							call_site_type_arguments,
							call_site: call_site.clone(),
						},
						parent_type_arguments,
						arguments,
						environment,
						behavior,
						types,
						// Very important!
						false,
					)?
					.returned_type;

				// TODO pass down
				let on = types.register_type(Type::Function(self.id, this_value));
				let new_type = Type::Constructor(Constructor::FunctionResult {
					on,
					with: with.clone(),
					result,
				});

				let ty = types.register_type(new_type);

				behavior.get_top_level_facts(environment).events.push(Event::CallsType {
					on,
					with: arguments.to_vec().into_boxed_slice(),
					reflects_dependency: Some(ty),
					timing: crate::events::CallingTiming::Synchronous,
					called_with_new,
					position: call_site.clone(),
				});

				return Ok(FunctionCallResult {
					returned_type: ty,
					warnings: Default::default(),
					called: None,
					special: None,
				});
			}
		}

		let (mut errors, mut warnings) = (Vec::new(), Vec::<()>::new());

		// Type arguments of the function
		let local_type_argument_as_restrictions: map_vec::Map<
			TypeId,
			Vec<(TypeId, SpanWithSource)>,
		> = if let (Some(call_site_type_arguments), true) =
			(call_site_type_arguments, E::CHECK_PARAMETERS)
		{
			self.synthesise_call_site_type_arguments(call_site_type_arguments, types, environment)
		} else {
			map_vec::Map::new()
		};

		let mut seeding_context = SeedingContext {
			type_arguments: map_vec::Map::new(),
			type_restrictions: local_type_argument_as_restrictions,
			locally_held_functions: map_vec::Map::new(),
			argument_position_and_parameter_idx: (SpanWithSource::NULL_SPAN, 0),
		};

		match self.behavior {
			FunctionBehavior::ArrowFunction { is_async } => {}
			FunctionBehavior::Method { free_this_id, .. } => {
				let value_of_this =
					this_value.get_passed().expect("method has no 'this' passed :?");
				crate::utils::notify!("ftid {:?} & vot {:?}", free_this_id, value_of_this);
				// TODO checking
				seeding_context
					.type_arguments
					.insert(free_this_id, vec![(value_of_this, SpanWithSource::NULL_SPAN, 0)]);
			}
			FunctionBehavior::Function { is_async, is_generator, free_this_id } => {
				match called_with_new {
					CalledWithNew::New { on } => {
						crate::utils::notify!("TODO set prototype");
						// if let Some(prototype) = non_super_prototype {
						// let this_ty = environment.create_this(prototype, types);
						let value_of_this =
							types.register_type(Type::Object(crate::types::ObjectNature::RealDeal));

						seeding_context.type_arguments.insert(
							// TODO
							free_this_id,
							vec![(value_of_this, SpanWithSource::NULL_SPAN, 0)],
						);
					}
					CalledWithNew::SpecialSuperCall { this_type } => todo!(),
					CalledWithNew::None => {
						// TODO
						let value_of_this = this_value.get(environment, types, &call_site);

						seeding_context.type_arguments.insert(
							free_this_id,
							vec![(value_of_this, SpanWithSource::NULL_SPAN, 0)],
						);
					}
				}
			}
			FunctionBehavior::Constructor { non_super_prototype, this_object_type } => {
				if let CalledWithNew::None = called_with_new {
					errors.push(FunctionCallingError::NeedsToBeCalledWithNewKeyword(
						call_site.clone(),
					));
				}
			}
		}

		{
			let import_new_argument = match called_with_new {
				CalledWithNew::New { on } => on,
				CalledWithNew::SpecialSuperCall { .. } => {
					todo!()
					// let ty = this_value.0;
					// let on = crate::types::printing::print_type(
					// 	ty,
					// 	types,
					// 	&environment.as_general_context(),
					// 	true,
					// );
					// crate::utils::notify!("This argument {}", on);
					// ty
				}
				// In spec == undefined
				CalledWithNew::None => TypeId::UNDEFINED_TYPE,
			};

			// TODO on type arguments, not seeding context
			seeding_context.type_arguments.insert(
				TypeId::NEW_TARGET_ARG,
				vec![(import_new_argument, SpanWithSource::NULL_SPAN, 0)],
			);
		}

		let SeedingContext {
			type_arguments: mut found,
			mut type_restrictions,
			mut locally_held_functions,
			argument_position_and_parameter_idx: _,
		} = self.synthesise_arguments::<E>(
			arguments,
			seeding_context,
			environment,
			types,
			&mut errors,
			&call_site,
		);

		// take the found and inject back into what it resolved
		let mut result_type_arguments = map_vec::Map::new();

		found.into_iter().for_each(|(item, values)| {
			let mut into_iter = values.into_iter();
			let (mut value, argument_position, param) =
				into_iter.next().expect("no type argument ...?");

			let restrictions_for_item = type_restrictions.get(&item);
			if let Some(restrictions_for_item) = restrictions_for_item {
				for (restriction, restriction_position) in restrictions_for_item {
					let mut behavior = BasicEquality {
						add_property_restrictions: false,
						position: restriction_position.clone(),
					};

					let result =
						type_is_subtype(*restriction, value, &mut behavior, environment, types);

					if let SubTypeResult::IsNotSubType(err) = result {
						let argument_type = TypeStringRepresentation::from_type_id(
							value,
							&environment.as_general_context(),
							types,
							false,
						);
						let restriction_type = TypeStringRepresentation::from_type_id(
							*restriction,
							&environment.as_general_context(),
							types,
							false,
						);

						let restriction = Some((restriction_position.clone(), restriction_type));
						let synthesised_parameter = &self.parameters.parameters[param];
						let parameter_type = TypeStringRepresentation::from_type_id(
							synthesised_parameter.ty,
							&environment.as_general_context(),
							types,
							false,
						);

						errors.push(FunctionCallingError::InvalidArgumentType {
							argument_type,
							argument_position: argument_position.clone(),
							parameter_type,
							parameter_position: synthesised_parameter.position.clone(),
							restriction,
						});
					}
				}
			}

			// TODO position is just the first
			result_type_arguments.insert(item, (value, argument_position));
		});
		// for (item, restrictions) in type_restrictions.iter() {
		// 	for (restriction, pos) in restrictions {
		// 		// TODO
		// 	}
		// }

		let mut type_arguments = FunctionTypeArguments {
			structure_arguments: parent_type_arguments,
			local_arguments: result_type_arguments,
			closure_id: None,
		};

		if E::CHECK_PARAMETERS {
			// TODO check free variables from inference
		}

		if !errors.is_empty() {
			return Err(errors);
		}

		// Evaluate effects directly into environment
		let mut early_return = behavior.new_function_target(self.id, |target| {
			type_arguments.closure_id = if self.closed_over_variables.is_empty() {
				None
			} else {
				let closure_id = types.new_closure_id();
				Some(closure_id)
			};

			let mut return_result = None;

			for event in self.effects.clone() {
				let result =
					apply_event(event, this_value, &mut type_arguments, environment, target, types);

				if let value @ Some(_) = result {
					return_result = value;
					break;
				}
			}

			if let Some(closure_id) = type_arguments.closure_id {
				// Set closed over values
				self.closed_over_variables.iter().for_each(|(reference, value)| {
					let value = substitute(*value, &mut type_arguments, environment, types);
					environment
						.facts
						.closure_current_values
						.insert((closure_id, reference.clone()), value);

					crate::utils::notify!("in {:?} set {:?} to {:?}", closure_id, reference, value);
				});
			}

			return_result
		});

		if let CalledWithNew::New { .. } = called_with_new {
			// TODO ridiculous early return primitive rule
			match self.behavior {
				FunctionBehavior::ArrowFunction { is_async } => todo!(),
				FunctionBehavior::Method { is_async, is_generator, .. } => todo!(),
				FunctionBehavior::Function { is_async, is_generator, free_this_id } => {
					let new_instance_type = type_arguments
						.local_arguments
						.remove(&free_this_id)
						.expect("no this argument7")
						.0;

					return Ok(FunctionCallResult {
						returned_type: new_instance_type,
						warnings: Default::default(),
						called: Some(self.id),
						special: None,
					});
				}
				FunctionBehavior::Constructor { non_super_prototype, this_object_type } => {
					crate::utils::notify!("Registered this {:?}", this_object_type);
					let new_instance_type = type_arguments
						.local_arguments
						.remove(&this_object_type)
						.expect("no this argument7")
						.0;

					return Ok(FunctionCallResult {
						returned_type: new_instance_type,
						warnings: Default::default(),
						called: Some(self.id),
						special: None,
					});
				}
			}
		}

		// set events should cover property specialisation here:
		let returned_type = if let Some(returned_type) = early_return {
			returned_type
		} else {
			substitute(self.return_type, &mut type_arguments, environment, types)
		};

		Ok(FunctionCallResult {
			returned_type,
			warnings: Default::default(),
			called: Some(self.id),
			special: None,
		})
	}

	fn synthesise_arguments<E: CallCheckingBehavior>(
		&self,
		arguments: &[SynthesisedArgument],
		mut seeding_context: SeedingContext,
		environment: &mut Environment,
		types: &TypeStore,
		errors: &mut Vec<FunctionCallingError>,
		call_site: &source_map::BaseSpan<SourceId>,
	) -> SeedingContext {
		for (parameter_idx, parameter) in self.parameters.parameters.iter().enumerate() {
			// TODO temp

			// This handles if the argument is missing but allowing elided arguments
			let argument = arguments.get(parameter_idx);

			let argument_type_and_pos = argument.map(|argument| {
				if let SynthesisedArgument::NonSpread { ty, position: pos } = argument {
					(ty, pos)
				} else {
					todo!()
				}
			});

			// let (auto_inserted_arg, argument_type) =
			// 	if argument_type.is_none() && checking_data.settings.allow_elided_arguments {
			// 		(true, Some(Cow::Owned(crate::types::Term::Undefined.into())))
			// 	} else {
			// 		(false, argument_type.map(Cow::Borrowed))
			// 	};

			if let Some((argument_type, argument_position)) = argument_type_and_pos {
				seeding_context.argument_position_and_parameter_idx =
					(argument_position.clone(), parameter_idx);

				// crate::utils::notify!(
				// 	"param {}, arg {}",
				// 	types.debug_type(parameter.ty),
				// 	types.debug_type(*argument_type)
				// );

				if E::CHECK_PARAMETERS {
					// crate::utils::notify!("Param {:?} :> {:?}", parameter.ty, *argument_type);
					let result = type_is_subtype(
						parameter.ty,
						*argument_type,
						&mut seeding_context,
						environment,
						types,
					);

					if let SubTypeResult::IsNotSubType(reasons) = result {
						errors.push(FunctionCallingError::InvalidArgumentType {
							parameter_type: TypeStringRepresentation::from_type_id(
								parameter.ty,
								&environment.as_general_context(),
								types,
								false,
							),
							argument_type: TypeStringRepresentation::from_type_id(
								*argument_type,
								&environment.as_general_context(),
								types,
								false,
							),
							parameter_position: parameter.position.clone(),
							argument_position: argument_position.clone(),
							restriction: None,
						});
					}
				} else {
					// Already checked so can set. TODO destructuring etc
					seeding_context.set_id(
						parameter.ty,
						(*argument_type, argument_position.clone(), parameter_idx),
						false,
					);
				}
			} else if let Some(value) = parameter.missing_value {
				// TODO evaluate effects & get position
				seeding_context.set_id(
					parameter.ty,
					(value, SpanWithSource::NULL_SPAN, parameter_idx),
					false,
				);
			} else {
				// TODO group
				errors.push(FunctionCallingError::MissingArgument {
					parameter_position: parameter.position.clone(),
					call_site: call_site.clone(),
				});
			}

			// if let SubTypeResult::IsNotSubType(_reasons) = result {
			// 	if auto_inserted_arg {
			// 		todo!("different error");
			// 	} else {
			// 		errors.push(FunctionCallingError::InvalidArgumentType {
			// 			parameter_index: idx,
			// 			argument_type: argument_type.into_owned(),
			// 			parameter_type: parameter_type.clone(),
			// 			parameter_pos: parameter.2.clone().unwrap(),
			// 		});
			// 	}
			// }
			// } else {
			// 	errors.push(FunctionCallingError::MissingArgument {
			// 		parameter_pos: parameter.2.clone().unwrap(),
			// 	});
			// }
		}
		if self.parameters.parameters.len() < arguments.len() {
			if let Some(ref rest_parameter) = self.parameters.rest_parameter {
				for (idx, argument) in
					arguments.iter().enumerate().skip(self.parameters.parameters.len())
				{
					let SynthesisedArgument::NonSpread {
						ty: argument_type,
						position: argument_pos,
					} = argument
					else {
						todo!()
					};

					let item_type = if let Type::Constructor(Constructor::StructureGenerics(
						StructureGenerics { on, arguments },
					)) = types.get_type_by_id(rest_parameter.item_type)
					{
						assert_eq!(*on, TypeId::ARRAY_TYPE);
						if let Some(item) = arguments.get_argument(TypeId::T_TYPE) {
							item
						} else {
							unreachable!()
						}
					} else {
						unreachable!()
					};

					if E::CHECK_PARAMETERS {
						let result = type_is_subtype(
							item_type,
							*argument_type,
							&mut seeding_context,
							environment,
							types,
						);

						if let SubTypeResult::IsNotSubType(reasons) = result {
							errors.push(FunctionCallingError::InvalidArgumentType {
								parameter_type: TypeStringRepresentation::from_type_id(
									rest_parameter.item_type,
									&environment.as_general_context(),
									types,
									false,
								),
								argument_type: TypeStringRepresentation::from_type_id(
									*argument_type,
									&environment.as_general_context(),
									types,
									false,
								),
								argument_position: argument_pos.clone(),
								parameter_position: rest_parameter.position.clone(),
								restriction: None,
							});
						}
					} else {
						todo!("substitute")
					}
				}
			} else {
				// TODO types.settings.allow_extra_arguments
				let mut left_over = arguments.iter().skip(self.parameters.parameters.len());
				let first = left_over.next().unwrap();
				let mut count = 1;
				let mut end = None;
				while let arg @ Some(_) = left_over.next() {
					count += 1;
					end = arg;
				}
				let position = if let Some(end) = end {
					first
						.get_position()
						.without_source()
						.union(end.get_position().without_source())
						.with_source(end.get_position().source)
				} else {
					first.get_position()
				};
				errors.push(FunctionCallingError::ExcessArguments { count, position });
			}
		}
		seeding_context
	}

	fn synthesise_call_site_type_arguments(
		&self,
		call_site_type_arguments: Vec<(TypeId, SpanWithSource)>,
		types: &crate::types::TypeStore,
		environment: &mut Environment,
	) -> map_vec::Map<TypeId, Vec<(TypeId, SpanWithSource)>> {
		if let Some(ref typed_parameters) = self.type_parameters {
			typed_parameters
				.0
				.iter()
				.zip(call_site_type_arguments)
				.map(|(param, (ty, pos))| {
					if let Type::RootPolyType(PolyNature::Generic { eager_fixed, .. }) =
						types.get_type_by_id(param.id)
					{
						let mut basic_subtyping = BasicEquality {
							add_property_restrictions: false,
							position: pos.clone(),
						};
						let type_is_subtype = type_is_subtype(
							*eager_fixed,
							ty,
							&mut basic_subtyping,
							environment,
							types,
						);

						match type_is_subtype {
							SubTypeResult::IsSubType => {}
							SubTypeResult::IsNotSubType(_) => {
								todo!("generic argument does not match restriction")
							}
						}
					} else {
						todo!();
						// crate::utils::notify!("Generic parameter with no aliasing restriction, I think this fine on internals");
					};

					(param.id, vec![(ty, pos)])
				})
				.collect()
		} else {
			crate::utils::notify!("Call site arguments on function without typed parameters...");
			map_vec::Map::new()
		}
	}
}
