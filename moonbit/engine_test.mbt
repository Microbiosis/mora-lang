// Copyright (c) 2026 Microbiosis. BSD-3-Clause. See the repository LICENSE.

let glance : String =
  #|task double_all(xs)
  #|  return xs.map(fn(x) x * 2 end)
  #|end
  #|
  #|let nums = [1, 2, 3, 4]
  #|let doubled = double_all(nums)
  #|print("doubled =", doubled)
  #|
  #|let classify = fn(n)
  #|  match n with
  #|    x when x > 0 -> "positive"
  #|    0 -> "zero"
  #|    _ -> "negative"
  #|  end
  #|end
  #|
  #|print(classify(5))
  #|print(classify(0))
  #|print(classify(0 - 3))

let typed : String =
  #|let name: string = "mora"
  #|let count: number = 42
  #|
  #|task add(a: number, b: number): number
  #|  return a + b
  #|end
  #|
  #|let total = add(count, 8)
  #|let inc = fn(x: number) x + 1 end
  #|print(inc(total))

test "readme glance" {
  let outcome = @mora_moon.run_source(glance)
  inspect(
    outcome.text(),
    content=(
      #|doubled =	[2, 4, 6, 8]
      #|positive
      #|zero
      #|negative
    ),
  )
}

test "readme type annotations" {
  let outcome = @mora_moon.run_source(typed)
  inspect(outcome.text(), content="51")
}

test "let polymorphism instantiates at each use" {
  let outcome = @mora_moon.run_source(
    "let id = fn(x) x end\nprint(id(1))\nprint(id(\"a\"))\nprint(id(true))\n",
  )
  inspect(
    outcome.text(),
    content=(
      #|1
      #|a
      #|true
    ),
  )
}

test "integer division stays integral and mixed arithmetic promotes" {
  let outcome = @mora_moon.run_source(
    "print(7 / 2)\nprint(7.0 / 2)\nprint(1 + 2.5)\n",
  )
  inspect(
    outcome.text(),
    content=(
      #|3
      #|3.5
      #|3.5
    ),
  )
}

test "string annotation rejects an int" {
  let message = try {
    @mora_moon.check_source("let name: string = 1\n")
    "accepted"
  } catch {
    err => err.message()
  } noraise {
    _ => "accepted"
  }
  inspect(message.contains("Type error at line 1"), content="true")
  inspect(message.contains("expected: string"), content="true")
  inspect(message.contains("actual:   int"), content="true")
}

test "division by zero is a runtime error" {
  let message = try {
    ignore(@mora_moon.run_source("print(1 / 0)\n"))
    "accepted"
  } catch {
    err => err.message()
  } noraise {
    _ => "accepted"
  }
  inspect(message.contains("division by zero"), content="true")
}

test "effects are rejected instead of being dropped" {
  let message = try {
    ignore(@mora_moon.run_source("let x = perform Flip()\n"))
    "accepted"
  } catch {
    err => err.message()
  } noraise {
    _ => "accepted"
  }
  inspect(message.contains("not in the MoonBit trial"), content="true")
}

test "float arithmetic binds an unannotated parameter" {
  let outcome = @mora_moon.run_source(
    "let f = fn(x) 1.0 + x end\nprint(f(2))\n",
  )
  inspect(outcome.text(), content="3.0")
  let message = try {
    ignore(@mora_moon.check_source("let f = fn(x) 1.0 + x end\nlet bad = f(\"a\")\n"))
    "accepted"
  } catch {
    err => err.message()
  } noraise {
    _ => "accepted"
  }
  inspect(message.contains("Type error"), content="true")
}

test "equal-length list addition is elementwise" {
  let outcome = @mora_moon.run_source("print([1, 2] + [10, 20])\nprint([1] + [2, 3])\n")
  inspect(
    outcome.text(),
    content=(
      #|[11, 22]
      #|[1, 2, 3]
    ),
  )
}
