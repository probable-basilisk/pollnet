#! /usr/bin/env python3

#The MIT License (MIT)
#
#Copyright <2022> <Alexandre Maia Godoi>
#
#Permission is hereby granted, free of charge, to any person obtaining a copy of
#this software and associated documentation files (the “Software”), to deal in
#the Software without restriction, including without limitation the rights to
#use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
#of the Software, and to permit persons to whom the Software is furnished to do so.

#THE SOFTWARE IS PROVIDED “AS IS”, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
#IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
#FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
#AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
#LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
#FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
#DEALINGS IN THE SOFTWARE.

from http.server import BaseHTTPRequestHandler, HTTPServer
import logging
import sys

COLOR = "\033[1;32m"
RESET_COLOR = "\033[00m"

class S(BaseHTTPRequestHandler):
    def _set_response(self):
        self.send_response(200)
        self.send_header('Content-type', 'text/html')
        self.end_headers()

    def do_log(self, method):
        content_length = self.headers['Content-Length']
        content_length = 0 if (content_length is None) else int(content_length)
        post_data = self.rfile.read(content_length)
        logging.info(COLOR + method + " request,\n" + RESET_COLOR + "Path: %s\nHeaders:\n%sBody:\n%s\n",
                str(self.path), str(self.headers), post_data.decode('utf-8'))
        self._set_response()
        self.wfile.write((method + " request for {}\n".format(self.path)).encode('utf-8'))
        agent = self.headers['User-Agent']
        if agent is not None:
            self.wfile.write(("agent: " + agent + "\n").encode('utf-8'))
        testtesttest = self.headers["X-Test-Test-Test"]
        if testtesttest is not None:
            self.wfile.write(testtesttest.encode('utf-8'))

    def do_GET(self):
        self.do_log("GET")

    def do_POST(self):
        self.do_log("POST")

    def do_PUT(self):
        self.do_log("PUT")

    def do_DELETE(self):
        self.do_log("DELETE")

def run(address, port, server_class=HTTPServer, handler_class=S):
    logging.basicConfig(level=logging.INFO)
    server_address = (address, port)
    httpd = server_class(server_address, handler_class)
    logging.info('Starting httpd...\n')
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    httpd.server_close()
    logging.info('Stopping httpd...\n')

if __name__ == '__main__':
    if len(sys.argv) != 3:
        print("Usage:\n" + sys.argv[0] + " [address] [port]")
        sys.exit(1)

    run(sys.argv[1], int(sys.argv[2]))
