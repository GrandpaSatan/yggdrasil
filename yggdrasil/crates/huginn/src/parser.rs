use ygg_domain::chunk::{ChunkType, Language};

/// Return the tree-sitter `Language` for a supported language.
/// Returns `None` for `Language::Yaml` and `Language::Unknown` — these are not
/// processed with tree-sitter.
#[must_use]
pub fn tree_sitter_language(lang: Language) -> Option<tree_sitter::Language> {
    match lang {
        Language::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        Language::Python => Some(tree_sitter_python::LANGUAGE.into()),
        Language::Go => Some(tree_sitter_go::LANGUAGE.into()),
        Language::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        Language::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        // TSX uses the same TypeScript grammar — we keep TypeScript as the single
        // Language variant for both .ts and .tsx (Language::from_extension maps both).
        Language::Markdown => Some(tree_sitter_md::LANGUAGE.into()),
        Language::Yaml | Language::Unknown => None,
    }
}

/// Return the tree-sitter query string for extracting semantic nodes.
/// The queries use named captures: `@function`, `@struct`, `@enum`, `@impl`,
/// `@trait`, `@module`, `@heading`. The capture name determines `ChunkType`.
#[must_use]
pub fn extraction_query(lang: Language) -> &'static str {
    match lang {
        Language::Rust => {
            r#"
(function_item
  name: (identifier) @fn_name) @function

(struct_item
  name: (type_identifier) @struct_name) @struct

(enum_item
  name: (type_identifier) @enum_name) @enum

(impl_item
  type: (type_identifier) @impl_type) @impl

(trait_item
  name: (type_identifier) @trait_name) @trait

(mod_item
  name: (identifier) @mod_name) @module
"#
        }
        Language::Python => {
            r#"
(function_definition
  name: (identifier) @fn_name) @function

(class_definition
  name: (identifier) @class_name) @struct

(decorated_definition
  definition: (function_definition
    name: (identifier) @fn_name)) @function
"#
        }
        Language::Go => {
            r#"
(function_declaration
  name: (identifier) @fn_name) @function

(method_declaration
  name: (field_identifier) @fn_name) @function

(type_declaration
  (type_spec
    name: (type_identifier) @struct_name
    type: (struct_type))) @struct

(type_declaration
  (type_spec
    name: (type_identifier) @trait_name
    type: (interface_type))) @trait
"#
        }
        Language::JavaScript => {
            r#"
(function_declaration
  name: (identifier) @fn_name) @function

(lexical_declaration
  (variable_declarator
    name: (identifier) @fn_name
    value: (arrow_function))) @function

(class_declaration
  name: (identifier) @class_name) @struct

(method_definition
  name: (property_identifier) @fn_name) @function

(export_statement
  declaration: (function_declaration
    name: (identifier) @fn_name)) @function

(export_statement
  declaration: (class_declaration
    name: (identifier) @class_name)) @struct
"#
        }
        Language::TypeScript => {
            r#"
(function_declaration
  name: (identifier) @fn_name) @function

(lexical_declaration
  (variable_declarator
    name: (identifier) @fn_name
    value: (arrow_function))) @function

(class_declaration
  name: (identifier) @class_name) @struct

(method_definition
  name: (property_identifier) @fn_name) @function

(interface_declaration
  name: (type_identifier) @trait_name) @trait

(type_alias_declaration
  name: (type_identifier) @struct_name) @struct

(export_statement
  declaration: (function_declaration
    name: (identifier) @fn_name)) @function

(export_statement
  declaration: (class_declaration
    name: (identifier) @class_name)) @struct
"#
        }
        // Markdown uses a heading query. Chunking strategy is handled separately in chunker.rs.
        Language::Markdown => {
            r#"
(atx_heading
  heading_content: (inline) @heading_text) @heading
"#
        }
        Language::Yaml | Language::Unknown => "",
    }
}

/// Walk up the AST from `node` to extract parent context (e.g., the enclosing
/// impl/trait/class signature).
///
/// Returns the first line of the enclosing scope's source text, or an empty
/// string if no meaningful parent context exists.
#[must_use]
pub fn extract_parent_context(node: tree_sitter::Node, source: &[u8], lang: Language) -> String {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        let kind = parent.kind();
        match lang {
            Language::Rust => {
                if kind == "impl_item" || kind == "trait_item" {
                    return first_line_of(parent, source);
                }
                if kind == "mod_item" {
                    return first_line_of(parent, source);
                }
            }
            Language::Python => {
                if kind == "class_definition" {
                    return first_line_of(parent, source);
                }
                if kind == "function_definition" {
                    return first_line_of(parent, source);
                }
            }
            Language::Go => {
                if kind == "method_declaration" {
                    // Extract the receiver type from the source line.
                    return first_line_of(parent, source);
                }
            }
            Language::TypeScript | Language::JavaScript => {
                if kind == "class_declaration" || kind == "class_body" {
                    return first_line_of(parent, source);
                }
            }
            _ => {}
        }
        cursor = parent.parent();
    }
    String::new()
}

/// Map a tree-sitter query capture name (the `@capture` label) to a `ChunkType`.
///
/// The outer capture (e.g., `@function`, `@struct`) determines the type.
/// Returns `None` for auxiliary captures like `@fn_name`, `@struct_name`, etc.
#[must_use]
pub fn node_kind_to_chunk_type(capture_name: &str) -> Option<ChunkType> {
    match capture_name {
        "function" => Some(ChunkType::Function),
        "struct" => Some(ChunkType::Struct),
        "enum" => Some(ChunkType::Enum),
        "impl" => Some(ChunkType::Impl),
        "trait" => Some(ChunkType::Trait),
        "module" => Some(ChunkType::Module),
        "heading" => Some(ChunkType::Documentation),
        _ => None,
    }
}

/// Extract the first line of a node's source text. Used for parent context signatures.
fn first_line_of(node: tree_sitter::Node, source: &[u8]) -> String {
    let text = node.utf8_text(source).unwrap_or("").trim();
    text.lines().next().unwrap_or("").trim().to_string()
}
