#!/bin/bash

usage() { echo "Usage: $0 [-s <45|90>] [-p <string>]" 1>&2; exit 1; }

while getopts "u:" o; do
    case "${o}" in
        u)
            username=${OPTARG}
mkdir output/${username} 
echo "Generating a report of deleted tweets for ${username}"
target/release/twcc deleted-tweets --report ${username} > output/${username}/deleted_tweets.md
echo "Finished. Please see `output/${username}/deleted_tweets.md`"
    esac
done