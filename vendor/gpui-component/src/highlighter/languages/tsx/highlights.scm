; JSX elements

(jsx_opening_element
  [
    (identifier) @type
    (member_expression
      object: (identifier) @type
      property: (property_identifier) @type)
  ])

(jsx_closing_element
  [
    (identifier) @type
    (member_expression
      object: (identifier) @type
      property: (property_identifier) @type)
  ])

(jsx_self_closing_element
  [
    (identifier) @type
    (member_expression
      object: (identifier) @type
      property: (property_identifier) @type)
  ])

(jsx_opening_element
  (identifier) @tag.jsx
  (#match? @tag.jsx "^[a-z][^.]*$"))

(jsx_closing_element
  (identifier) @tag.jsx
  (#match? @tag.jsx "^[a-z][^.]*$"))

(jsx_self_closing_element
  (identifier) @tag.jsx
  (#match? @tag.jsx "^[a-z][^.]*$"))

(jsx_attribute (property_identifier) @attribute.jsx)

(jsx_opening_element (["<" ">"] @punctuation.bracket.jsx))
(jsx_closing_element (["</" ">"] @punctuation.bracket.jsx))
(jsx_self_closing_element (["<" "/>"] @punctuation.bracket.jsx))
(jsx_attribute "=" @punctuation.delimiter.jsx)

(html_character_reference) @string.special
