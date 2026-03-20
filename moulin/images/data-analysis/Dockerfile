FROM python:3.12-slim

RUN mkdir -p /home/moulin/workspace
WORKDIR /home/moulin/workspace

ENV PIP_INDEX_URL=https://pypi.tuna.tsinghua.edu.cn/simple

RUN pip install --no-cache-dir \
    pandas \
    numpy \
    openpyxl

ENTRYPOINT ["sleep", "infinity"]
