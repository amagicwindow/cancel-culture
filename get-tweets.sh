#!/bin/bash
while getopts "u:" o; do
    case "${o}" in
        u)
            username=${OPTARG}
            echo "Generating a report of deleted tweets for ${username}"
            target/release/twcc deleted-tweets --report ${username} > output/${username}_deleted_tweets.md
            echo "Finished. Please see /output/${username}/deleted_tweets.md"
    esac
done