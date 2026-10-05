from unittest import mock

from notifier import email


def test_send_receipt_uses_receipt_template():
    with mock.patch.object(email, "render", return_value="<p>hi</p>") as render, \
            mock.patch.object(email, "send", return_value="msg_1"):
        assert email.send_receipt("inv_1", "a@example.com", 1200) == "msg_1"
    render.assert_called_once_with("receipt", invoice_id="inv_1", total_minor=1200)
