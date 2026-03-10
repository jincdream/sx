FROM python:3.9-slim

RUN mkdir -p /home/daytona/workspace
WORKDIR /home/daytona/workspace

RUN pip install --no-cache-dir \
    pandas \
    numpy \
    openpyxl

ENTRYPOINT ["sleep", "infinity"]
