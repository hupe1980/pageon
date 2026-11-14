//! Incremental parsing that stops at each `</script>`, so a script engine can run the script
//! with the document exactly as parsed up to that point, as browsers do.
//!
//! ```
//! use pageon_dom::{Parser, ParseOptions, Step};
//!
//! let mut parser = Parser::new(ParseOptions { scripting: true });
//! parser.feed("<p>a</p><script>x()</script><p>b</p>");
//! let mut scripts = 0;
//! while let Step::Script(_node) = parser.step() {
//!     scripts += 1; // run the script here; the DOM so far is `parser.dom()`
//! }
//! let dom = parser.finish();
//! assert_eq!((scripts, dom.borrow().text(dom.borrow().root()).as_str()), (1, "a\nb"));
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use html5ever::TokenizerResult;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Tokenizer};
use html5ever::tree_builder::TreeBuilder;

use crate::dom::{Dom, NodeId};
use crate::sink::{ParseOptions, Sink, opts};

/// What the parser reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A `<script>` element was just closed: run it, then call [`Parser::step`] again.
    Script(NodeId),
    /// All input fed so far is parsed.
    Done,
}

/// An HTML document parser that pauses at scripts.
pub struct Parser {
    tokenizer: Tokenizer<TreeBuilder<NodeId, Sink>>,
    input: BufferQueue,
    dom: Rc<RefCell<Dom>>,
}

impl std::fmt::Debug for Parser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parser").finish_non_exhaustive()
    }
}

impl Parser {
    /// A parser for a new document.
    pub fn new(options: ParseOptions) -> Self {
        let dom = Rc::new(RefCell::new(Dom::new()));
        let o = opts(options);
        let builder = TreeBuilder::new(
            Sink {
                dom: dom.clone(),
                fragment: false,
            },
            o.tree_builder,
        );
        Self {
            tokenizer: Tokenizer::new(builder, o.tokenizer),
            input: BufferQueue::default(),
            dom,
        }
    }

    /// The document being built; shared, so it can be read and changed while the parser is
    /// paused at a script.
    pub fn dom(&self) -> Rc<RefCell<Dom>> {
        self.dom.clone()
    }

    /// Appends input.
    pub fn feed(&mut self, text: &str) {
        self.input.push_back(StrTendril::from(text));
    }

    /// Parses until the next script end tag or the end of the input fed so far.
    pub fn step(&mut self) -> Step {
        self.step_inner()
    }

    /// Ends the input (closing open elements) and returns the document. Scripts reached only
    /// now are not paused at.
    pub fn finish(self) -> Rc<RefCell<Dom>> {
        while let Step::Script(_) = self.step_inner() {}
        self.tokenizer.end();
        self.dom
    }

    fn step_inner(&self) -> Step {
        loop {
            match self.tokenizer.feed(&self.input) {
                TokenizerResult::Script(node) => return Step::Script(node),
                TokenizerResult::Done => return Step::Done,
                // The bytes were already decoded with the right encoding.
                TokenizerResult::EncodingIndicator(_) => {}
            }
        }
    }
}
