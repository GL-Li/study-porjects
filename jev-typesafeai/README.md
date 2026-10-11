## Documentation & cookbook

https://docs.typesafe.ai/cookbooks

## Concepts

### System One model
Models that make fast, structured decisions for softward.


### state
State is the content you ask a system one model to evaluate. It can be a simple string or a structured Json value. 

Examples:
- a string: `state = "My card was charged twice.`
- a list of strings: `["Hi", "My customer number is TS1337.", "My card was charged twice."]`
- a json object: `{"message": "My card was charged twice.", "order_id": "A-104"}`

The following conversation is one state
```
{
  "ticket": {
    "subject": "Duplicate charge",
    "messages": [
      {"from": "customer", "text": "I was charged twice for order A-104. Please refund the duplicate."},
      {"from": "support", "text": "We are checking the charges."}
    ]
  },
  "order": {
    "id": "A-104",
    "charges": [
      {"amount_usd": 49, "status": "captured"},
      {"amount_usd": 49, "status": "captured"}
    ]
  },
  "refund_policy": "Duplicate charges are eligible for a refund."
}
```

### primitives (questions)
The types of questions (primitives) with structured answers:
- `choice`
- `score`: an ordered factor that measures. For custom satisfaction, we can have 0: unhappy, 1: neutral, 2: good, 3: excellent. A score of 2.4 mean the custom thinks the service is between good and excellent.
- `noul`: strictly "yes" or "no", or true or false judgement, with a probability but do NOT interpret the probability as a score. If you want to measure, use `score` above.

Every question has a `ID`, a `type`, `instructions`, and `criteria`. `criteria` is optional for noul question.
- `ID`: a string key you pick, such as "refund_requested"
- `type`: one of `choice`, `score`, or `noul`
- `instructions`: the question you ask about the state
- `criteria`: possible answers

Examples:

```python
from typesafe_sdk import noul
question = {
    "refund_requested": Noul( # type is noul
        instructions="Does the customer request a refunc",
    )
}
```

### quote state in primitives
Use the format `state_id.field_x....` in primitives

```python
questions = {
    "refund_requested": {
        "type": "noul",
        "instructions": "Does `ticket.messages[0].text` request a refund?",
    },
    "policy_supports_refund": {
        "type": "noul",
        "instructions": (
            "Does `refund_policy` support the refund requested "
            "in `ticket.messages[0].text`, given `order.charges`?"
        ),
    },
}
```

### One state, multiple questions in parallel 

Raw json format

```json
{
  "state": "Our API integration started returning 500 errors on every request about 20 minutes ago, and we can't process any customer orders until this is fixed.",
  "questions": {
    "department": {
      "type": "choice",
      "instructions": "Which team should handle this",
      "criteria": {
        "billing": "Payment or subscription issues",
        "technical": "Bugs or integration problems",
        "sales": "Pricing or account questions"
      }
    },
    "is_urgent": {
      "type": "noul",
      "instructions": "The message conveys urgency or time-sensitivity"
    },
    "frustration": {
      "type": "score",
      "instructions": "How frustrated the customer appears",
      "criteria": [
        "Calm, just stating facts",
        "Frustrated but civil",
        "Very angry, strong language"
      ]
    }
  }
}
```

Python: see `basics_1.py`
