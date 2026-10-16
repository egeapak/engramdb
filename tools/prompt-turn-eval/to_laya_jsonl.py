"""Convert a prompt_turn_*.json file to laya-train JSONL rows."""
import json, sys
QUESTION = {"work": {"type": "noul", "instructions": "Does the user ask the assistant to do work, including approving or choosing work the assistant proposed?"}}

def tail(text, n=300):
    if len(text) <= n:
        return text
    cut = text[-n:]
    i = cut.find(" ")
    return cut[i + 1:] if i >= 0 else cut

with open(sys.argv[2], "w") as out:
    for item in json.load(open(sys.argv[1]))["items"]:
        row = {"state": f"Assistant: {tail(item['last'])}\nUser: {item['prompt']}",
               "questions": QUESTION, "expected": {"work": bool(item["search"])}}
        out.write(json.dumps(row) + "\n")
