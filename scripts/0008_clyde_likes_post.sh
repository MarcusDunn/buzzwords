curl -v 'localhost:3000/user/marcus/post/My%201st%20Post/like' -H 'Content-Type: application/json' --data '{
  "liker": "clyde"
}'