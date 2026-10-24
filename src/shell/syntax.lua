-- Default syntax highlighting definitions for NeonEmacs (§5, §12)

syntax.register("rust", {
    extensions = { "rs" },
    line_comment = "//",
    keywords = {
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
        "trait", "true", "type", "unsafe", "use", "where", "while"
    }
})

syntax.register("c", {
    extensions = { "c", "h", "cpp", "hpp", "cc" },
    line_comment = "//",
    keywords = {
        "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
        "enum", "extern", "float", "for", "goto", "if", "int", "long", "register", "return",
        "short", "signed", "sizeof", "static", "struct", "switch", "typedef", "union",
        "unsigned", "void", "volatile", "while"
    }
})

syntax.register("python", {
    extensions = { "py" },
    line_comment = "#",
    keywords = {
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
        "True", "try", "while", "with", "yield"
    }
})

syntax.register("javascript", {
    extensions = { "js", "ts", "jsx", "tsx" },
    line_comment = "//",
    keywords = {
        "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
        "default", "delete", "do", "else", "export", "extends", "false", "finally", "for",
        "function", "if", "import", "in", "instanceof", "let", "new", "null", "return",
        "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void",
        "while", "with", "yield"
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
    }
})

syntax.register("shell", {
    extensions = { "sh", "bash" },
    line_comment = "#",
    keywords = {}
})

syntax.register("markdown", {
    extensions = { "md", "markdown" },
    keywords = {}
})
