use crate::ir::{Ir, Entry};
use proc_macro2::TokenStream;
use std::collections::HashMap;
use syn;
use syn::parse::{self, Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{braced, token, Attribute, Ident, LitStr, Token, Visibility};

pub(crate) type Result<T> = ::std::result::Result<T, TokenStream>;

/// Abstract syntax tree (AST) for the surface syntax of the `layout!` macro.
///
/// The surface syntax of the `layout!` macro takes the form:
///
/// ```
/// Attribute* Visibility "struct" Ident Directory
/// ```
///
/// This AST gets lowered by the `flatten` method to a flat list of entries,
/// organized by entry type. See the `Ir` type for details.
pub(crate) struct Ast {
    attrs: Vec<Attribute>,
    visibility: Visibility,
    name: Ident,
    directory: Directory,
}

impl Parse for Ast {
    fn parse(input: ParseStream) -> parse::Result<Self> {
        let attrs: Vec<Attribute> = input.call(Attribute::parse_outer)?;
        let visibility: Visibility = input.parse()?;
        input.parse::<Token![struct]>()?;
        let name: Ident = input.parse()?;
        let directory: Directory = input.parse()?;
        Ok(Ast { attrs, visibility, name, directory })
    }
}

impl Ast {
    /// Lowers the AST to a flattened intermediate representation.
    pub(crate) fn flatten(self) -> Result<Ir> {
        let mut results = Ir {
            name: self.name,
            attrs: self.attrs,
            visibility: self.visibility,
            dirs: vec![],
            files: vec![],
            exes: vec![],
        };

        self.directory.flatten(&mut results, vec![])?;

        Ok(results)
    }
}

/// Represents a directory entry in the AST, which can recursively contain
/// more entries.
///
/// The surface syntax of a directory takes the form:
///
/// ```
/// {
///     (FieldPrefix)FieldContents*
/// }
/// ```
struct Directory {
    brace_token: token::Brace,
    entries: Punctuated<FieldPrefix, FieldContents>,
}

impl Parse for Directory {
    fn parse(input: ParseStream) -> parse::Result<Self> {
        let content;
        Ok(Directory {
            brace_token: braced!(content in input),
            entries: content.parse_terminated(FieldPrefix::parse)?,
        })
    }
}

enum EntryKind {
    Exe,
    File,
    Dir,
}

impl Directory {
    /// Lowers the directory to a flattened intermediate representation.
    fn flatten(self, results: &mut Ir, context: Vec<LitStr>) -> Result<()> {
        let mut visited_entries = HashMap::new();

        for pair in self.entries.into_pairs() {
            let (prefix, punc) = pair.into_tuple();

            let mut entry = Entry {
                name: prefix.name,
                context: context.clone(),
                filename: prefix.filename.clone(),
            };

            match punc {
                Some(FieldContents::Dir(dir)) => {
                    let filename = prefix.filename.value();

                    if filename.ends_with(".exe") {
                        let error = syn::Error::new(
                            prefix.filename.span(),
                            "the `.exe` extension is not allowed for directory names",
                        );
                        return Err(error.to_compile_error());
                    }

                    if let Some(kind) = visited_entries.get(&filename) {
                        let message = match kind {
                            EntryKind::Exe => {
                                format!("filename `{}` is a duplicate of `{}` executable on non-Windows operating systems", filename, filename)
                            }
                            _ => {
                                format!("duplicate filename `{}`", filename)
                            }
                        };
                        let error = syn::Error::new(prefix.filename.span(), message);
                        return Err(error.to_compile_error());
                    }

                    visited_entries.insert(filename.clone(), EntryKind::Dir);

                    results.dirs.push(entry);
                    let mut sub_context = context.clone();
                    sub_context.push(prefix.filename);
                    dir.flatten(results, sub_context)?;
                }
                _ => {
                    let filename = prefix.filename.value();
                    if filename.ends_with(".exe") {
                        let filename = &filename[0..filename.len() - 4];

                        if let Some(kind) = visited_entries.get(filename) {
                            let message = match kind {
                                EntryKind::Exe => {
                                    format!("duplicate filename `{}.exe`", filename)
                                }
                                EntryKind::File => {
                                    format!("executable `{}` (on non-Windows operating systems) is a duplicate of `{}` filename", filename, filename)
                                }
                                EntryKind::Dir => {
                                    format!("executable `{}` (on non-Windows operating systems) is a duplicate of `{}` directory name", filename, filename)
                                }
                            };
                            let error = syn::Error::new(prefix.filename.span(), message);
                            return Err(error.to_compile_error());
                        }

                        visited_entries.insert(filename.to_string(), EntryKind::Exe);
                        entry.filename = LitStr::new(filename, prefix.filename.span());
                        results.exes.push(entry);
                    } else {
                        if let Some(kind) = visited_entries.get(&filename) {
                            let message = match kind {
                                EntryKind::Exe => {
                                    format!("filename `{}` is a duplicate of `{}` executable on non-Windows operating systems", filename, filename)
                                }
                                _ => {
                                    format!("duplicate filename `{}`", filename)
                                }
                            };
                            let error = syn::Error::new(prefix.filename.span(), message);
                            return Err(error.to_compile_error());
                        }

                        visited_entries.insert(filename, EntryKind::File);
                        results.files.push(entry);
                    }
                }
            }
        }
        Ok(())
    }
}

/// AST for the common prefix of a single field in a `layout!` struct declaration,
/// which is of the form:
///
/// ```
/// LitStr ":" Ident
/// ```
///
/// This is followed either by a semicolon (`;`), indicating that the field is a
/// file, or a braced directory entry, indicating that the field is a directory.
///
/// If the `LitStr` contains the suffix `".exe"`, it is treated specially as an
/// executable file, whose suffix (or lack thereof) is determined by the current
/// operating system (using the `std::env::consts::EXE_SUFFIX` constant).
struct FieldPrefix {
    filename: LitStr,
    colon: Token![:],
    name: Ident,
}

impl Parse for FieldPrefix {
    fn parse(input: ParseStream) -> parse::Result<Self> {
        Ok(FieldPrefix {
            filename: input.parse()?,
            colon: input.parse()?,
            name: input.parse()?,
        })
    }
}

/// AST for the suffix of a field in a `layout!` struct declaration.
enum FieldContents {
    /// A file field suffix, which consists of a single semicolon (`;`).
    File(Token![;]),

    /// A directory field suffix, which consists of a braced directory.
    Dir(Directory),
}

impl Parse for FieldContents {
    fn parse(input: ParseStream) -> parse::Result<Self> {
        let lookahead = input.lookahead1();
        Ok(if lookahead.peek(Token![;]) {
            let semi = input.parse()?;
            FieldContents::File(semi)
        } else {
            let directory = input.parse()?;
            FieldContents::Dir(directory)
        })
    }
}
