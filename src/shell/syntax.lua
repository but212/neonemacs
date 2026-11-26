-- Default syntax highlighting definitions for NeonEmacs (§5, §12)

syntax.register("rust", {
    extensions = { "rs" },
    line_comment = "//",
    single_quote_char_only = true,
    keywords = {
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
        "trait", "true", "type", "unsafe", "use", "where", "while"
    },
    types = {
        "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize",
        "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64",
        "String", "Option", "Result", "Vec", "Box"
    }
})

syntax.register("c", {
    extensions = { "c", "h", "cpp", "hpp", "cc", "cxx" },
    line_comment = "//",
    keywords = {
        "auto", "break", "case", "catch", "class", "const", "constexpr", "continue", "default",
        "delete", "do", "else", "enum", "explicit", "export", "extern", "false", "for", "goto",
        "if", "inline", "mutable", "namespace", "new", "noexcept", "nullptr", "operator",
        "override", "private", "protected", "public", "register", "return", "sizeof", "static",
        "struct", "switch", "template", "this", "throw", "true", "try", "typedef", "typename",
        "union", "using", "virtual", "volatile", "while"
    },
    types = {
        "bool", "char", "double", "float", "int", "long", "short", "signed",
        "size_t", "unsigned", "void"
    }
})

syntax.register("python", {
    extensions = { "py", "pyi" },
    line_comment = "#",
    keywords = {
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
        "True", "try", "while", "with", "yield"
    },
    types = {
        "bool", "bytes", "dict", "float", "int", "list", "object", "set", "str", "tuple"
    }
})

syntax.register("javascript", {
    extensions = { "js", "ts", "jsx", "tsx" },
    line_comment = "//",
    keywords = {
        "as", "async", "await", "break", "case", "catch", "class", "const", "continue",
        "debugger", "declare", "default", "delete", "do", "else", "enum", "export", "extends",
        "false", "finally", "for", "function", "if", "implements", "import", "in", "instanceof",
        "interface", "is", "keyof", "let", "namespace", "new", "null", "readonly", "return",
        "super", "switch", "this", "throw", "true", "try", "type", "typeof", "var", "void",
        "while", "with", "yield"
    },
    types = {
        "any", "bigint", "boolean", "never", "number", "string", "symbol", "unknown",
        "Array", "Promise", "Record"
    }
})

syntax.register("lua", {
    extensions = { "lua" },
    line_comment = "--",
    keywords = {
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
        "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true",
        "until", "while"
    }
})

syntax.register("go", {
    extensions = { "go" },
    line_comment = "//",
    keywords = {
        "break", "case", "chan", "const", "continue", "default", "defer", "else",
        "fallthrough", "for", "func", "go", "goto", "if", "import", "interface",
        "map", "package", "range", "return", "select", "struct", "switch", "type", "var"
    },
    types = {
        "bool", "byte", "complex64", "complex128", "error", "float32", "float64",
        "int", "int8", "int16", "int32", "int64", "rune", "string",
        "uint", "uint8", "uint16", "uint32", "uint64", "uintptr"
    }
})

syntax.register("shell", {
    extensions = { "sh", "bash", "zsh" },
    filenames = { ".bashrc", ".zshrc", ".bash_profile", ".profile" },
    line_comment = "#",
    keywords = {
        "case", "do", "done", "echo", "elif", "else", "esac", "exit", "export", "fi",
        "for", "function", "if", "in", "local", "return", "select", "then", "time",
        "until", "while"
    }
})

syntax.register("makefile", {
    filenames = { "Makefile", "makefile", "GNUmakefile" },
    line_comment = "#",
    keywords = {
        "define", "else", "endef", "endif", "export", "ifeq", "ifneq", "include",
        "override", "unexport"
    }
})

syntax.register("sql", {
    extensions = { "sql" },
    line_comment = "--",
    case_insensitive = true,
    keywords = {
        "add", "all", "alter", "and", "as", "by", "case", "check", "column", "create",
        "default", "delete", "distinct", "drop", "else", "end", "foreign", "from", "group",
        "having", "in", "index", "inner", "insert", "into", "is", "join", "key", "left",
        "limit", "not", "null", "offset", "on", "or", "order", "outer", "primary",
        "references", "right", "select", "table", "then", "union", "update", "values",
        "when", "where", "with"
    },
    types = {
        "bigint", "blob", "boolean", "char", "date", "datetime", "decimal", "double",
        "float", "int", "integer", "json", "numeric", "smallint", "text", "timestamp",
        "tinyint", "varchar"
    }
})

syntax.register("toml", {
    extensions = { "toml" },
    filenames = { "Cargo.lock" },
    line_comment = "#",
    keywords = { "false", "true" }
})

syntax.register("json", {
    extensions = { "json" },
    keywords = { "false", "null", "true" }
})

syntax.register("yaml", {
    extensions = { "yaml", "yml" },
    line_comment = "#",
    keywords = { "false", "no", "null", "true", "yes" }
})

syntax.register("markdown", {
    extensions = { "md", "markdown" },
    heading = "#"
})
