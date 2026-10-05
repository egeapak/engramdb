"""Follow-up when a customer's payment fails."""
from src.jobs.queue import enqueue


def schedule_retries(invoice):
    """Plan the payment retries for an invoice whose charge just failed, and enqueue each one."""
    raise NotImplementedError
