Provides 2 simple primitives:

`oneshot` and `conversation` functions, which follow standard logic for llm interactions, that most providers share.

Then the model is automatically chosen based on whether we care about cost/speed/quality. Currently this is expressed by choosing `Model::`{`Fast`/`Medium`/`Slow`}, from which we pick a model as hardcoded in current implementation. 
