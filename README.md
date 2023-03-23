## Features

- [ ] Ability to store information about users. including:
  - name
  - username
  - email
  - password
  - date of birth
  - list of friends
- [ ] Ability to store information about posts, including:
  - title
  - content 
  - author
  - date of creation
  - number of likes and comments.
- [ ] Ability to store information about comments, including
  - content
  - author
  - date of creation
  - number of likes
- [ ] Ability to track notifications for users, including
  - notifications for new posts
  - comments
  - and likes
- [ ] Ability to generate reports on user activity,
  - including posts created
  - comments created
  - likes given and received

## Setup

You need mongodb, rabbitmq, and redis running.

```bash 
docker run -d -p 27017:27017 -e MONGO_INITDB_DATABASE=buzzwords --name mongo mongo
docker run -d -p 5672:5672 --name rabbitmq rabbitmq
docker run -d -p 6379:6379 --name redis redis
```