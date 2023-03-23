curl -v localhost:3000/user/marcus/post/My%201st%20Post/comment/new -H 'Content-Type: application/json' --data '{
  "content": "clyde",
  "author": "This is a comment"
}'