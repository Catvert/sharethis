#!/bin/bash

# Create the database if it doesn't exist
mkdir -p /app/data

sqlx db setup

# Start the application
./sharethis
