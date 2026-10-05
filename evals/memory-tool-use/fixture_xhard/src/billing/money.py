from dataclasses import dataclass


@dataclass(frozen=True)
class Money:
    cents: int
    currency: str = "USD"

    def __add__(self, other):
        assert self.currency == other.currency
        return Money(self.cents + other.cents, self.currency)
