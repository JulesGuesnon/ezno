use serde::{de::Visitor, Deserialize};
use serde_json::Value;

// Module list: https://www.typescriptlang.org/docs/handbook/modules/theory.html#the-module-output-format
#[derive(PartialEq, Debug)]
pub enum Module {
	Node16,
	NodeNext,
	Es2015,
	Es2020,
	Es2022,
	EsNext,
	CommonJs,
	System,
	Amd,
	Umd,
}

impl Module {
	pub fn has_explicit_file_extension(&self) -> bool {
		matches!(self, Module::Node16 | Module::NodeNext)
	}
}

// Serde doesn't have a case insensitive macro, so implemting it
impl<'de> Deserialize<'de> for Module {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		struct ModuleVisitor {}

		impl<'de> Visitor<'de> for ModuleVisitor {
			type Value = Module;

			fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
				formatter.write_str("Expected a Module")
			}

			fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
			where
				E: serde::de::Error,
			{
				println!("Sir? {}", v);

				match &v.to_lowercase()[..] {
					"node16" => Ok(Module::Node16),
					"nodenext" => Ok(Module::NodeNext),
					"es2015" => Ok(Module::Es2015),
					"es2020" => Ok(Module::Es2020),
					"esnext" => Ok(Module::EsNext),
					"commonjs" => Ok(Module::CommonJs),
					"system" => Ok(Module::System),
					"amd" => Ok(Module::Amd),
					"umd" => Ok(Module::Umd),
					s => {
						Err(E::custom(format!("Invalid value '{}' for compilerOptions.module", s)))
					}
				}
			}
		}

		deserializer.deserialize_string(ModuleVisitor {})
	}
}

#[derive(Deserialize, Default, Debug)]
pub struct Tsconfig {
	pub module: Option<Module>,
}

impl Tsconfig {
	pub fn new(root: serde_json::Value) -> Self {
		match root {
			Value::Object(mut root) => {
				let mut tsconfig = Self::default();

				let compiler_options = match root.remove("compilerOptions") {
					Some(Value::Object(v)) => Some(v),
					_ => None,
				};

				if let Some(mut compiler_options) = compiler_options {
					tsconfig.set_module_opt(
						compiler_options
							.remove("module")
							.and_then(|m| serde_json::from_value(m).ok()),
					);
				}

				tsconfig
			}
			_ => panic!("Expected tsconfig to be an object"),
		}
	}

	fn set_module_opt(&mut self, module: Option<Module>) -> &mut Self {
		if let Some(module) = module {
			self.module = Some(module);
		}
		self
	}
}
