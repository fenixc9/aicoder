FROM python:3.11.11-slim-bookworm
RUN apt-get update -qq && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*
WORKDIR /testbed
RUN git init -q && git config user.email fixture@example.invalid && git config user.name fixture && printf '0\n' > value.txt && git add value.txt && git -c commit.gpgsign=false commit -qm baseline
COPY check.py /fixture_check.py
LABEL aicoder.fixture=swebench-5.0.2
