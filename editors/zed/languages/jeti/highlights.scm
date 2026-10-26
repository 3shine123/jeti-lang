; Jeti syntax highlighting for Zed — captures follow Zed's theme names
; (https://docs.zed.dev/themes#theme-colors).

; ---------------------------------------------------------------- comments

(comment) @comment

; -------------------------------------------------------------- preprocessor

; clang's classification, mirrored: the directive itself is a keyword, the
; include path is a string (green), and a macro's name is an identifier —
; ALL-CAPS names read as `@constant` through the rule in the types section.
[
  "#include" "#import" "#define" "#undef"
  "#if" "#ifdef" "#ifndef" "#elif" "#else" "#endif"
  "#pragma" "#error" "#warning" "#line" "#include_next"
] @keyword

(preproc_include path: (_) @string)
(preproc_def name: (identifier) @constant)
(preproc_undef name: (identifier) @constant)
(preproc_ifdef name: (identifier) @constant)

; ------------------------------------------------------------------ strings

(string_literal) @string
(objc_string_literal) @string
(system_lib_string) @string
(char_literal) @character

; ------------------------------------------------------------------ literals

(number_literal) @number
; `nil` is the builtin; YES/NO/NULL are ALL-CAPS and read `@constant`, as in
; clang's ObjC query.
((identifier) @constant.builtin
  (#match? @constant.builtin "^nil$"))

; ------------------------------------------------------------------ keywords

[
  "if" "else" "while" "for" "do" "switch" "case" "default"
  "return" "break" "continue" "goto"
  "typedef" "struct" "union" "enum"
  "sizeof" "static" "extern" "register" "auto"
  "const" "volatile" "restrict"
  "nullable" "nonnull" "_Nullable" "_Nonnull" "_Null_unspecified"
  "__typeof__" "__typeof" "typeof" "__attribute__" "__asm__" "asm"
  "oneway" "bycopy" "byref"
  "__block" "__weak" "__strong" "__autoreleasing"
  "when"
] @keyword

[
  "@try" "@catch" "@finally" "@defer"
] @keyword.control.exception

(await_expression "await") @keyword.control.await
(async_block_expression "async") @keyword.control.asynchronous

; ObjC compiler directives.
[
  "@interface" "@implementation" "@protocol" "@end"
  "@property" "@synthesize" "@dynamic"
  "@public" "@private" "@protected"
  "@required" "@optional"
  "@selector" "@throw" "@autoreleasepool" "@synchronized"
  "@class" "@namespace" "@using"
  "throws"
] @keyword.directive

(endnamespace_directive) @keyword.directive

(self_expression) @variable.builtin
(super_expression) @variable.builtin

; --------------------------------------------------------------------- types

; Types: builtin spellings, and capitalized user names — Jeti Foundation
; classes are all NP-prefixed, so this catches them and any Jeti class.
(type_name) @type
(struct_specifier "struct" @keyword.type)
(union_specifier "union" @keyword.type)
(enum_specifier "enum" @keyword.type)

(identifier) @variable
((identifier) @type
  (#match? @type "^[A-Z]"))
((type_name) @type
  (#match? @type "^[A-Z]"))

; ALL-CAPS names are macro-ish constants — clang's ObjC query does the same
; (`^_*[A-Z][A-Z\d_]*$`), and it must come after the `@type` rules so it wins
; for names like `MAX_CHILDREN`. YES/NO/NULL land here too, like in clang.
((identifier) @constant
  (#match? @constant "^_*[A-Z][A-Z\\d_]*$"))

; ------------------------------------------------------------- function calls

(call_expression
  function: (identifier) @function.call)

; ObjC selectors as call-ish highlights.
(message_expression
  (identifier) @function.method.call)

(method_declaration
  (identifier) @function.method)

(function_definition
  (identifier) @function)

; ------------------------------------------------------------ operators & punctuation

[
  "+" "-" "*" "/" "%" "=" "==" "!=" "<" ">" "<=" ">="
  "&&" "||" "!" "&" "|" "^" "~" "<<" ">>"
  "+=" "-=" "*=" "/=" "%=" "&=" "|=" "^=" "<<=" ">>="
  "++" "--" "?" ":"
] @operator

[
  "(" ")" "[" "]" "{" "}"
] @punctuation.bracket

[
  "," ";"
] @punctuation.delimiter

"." @operator
"->" @operator

; ------------------------------------------------------------------ properties

(field_expression
  field: (identifier) @property)

(objc_property_declaration
  (identifier) @property)
